//! How playback sounds: loudness levelled between songs from YouTube's own
//! loudness data, the equalizer, and the sleep timer's fade.
//!
//! Each song's gain is an mpv per-file `volume-gain`, so it holds exactly
//! for that song, including a gapless handoff to the song queued behind.
//! The equalizer is one labelled lavfi graph in `af`: band edits go to the
//! running graph with `af-command` (no gap) and `af` itself is rewritten
//! once edits stop, so a graph mpv rebuilds starts from the same values.

use serde_json::Value;

use super::*;
use crate::equalizer::LABEL;
use crate::model::SleepTimer;

/// The level songs are evened out to, in LKFS.
const TARGET_LKFS: f64 = -14.0;
/// Quiet songs are raised at most this much, to stay clear of clipping.
const MAX_BOOST_DB: f64 = 3.0;
/// The sleep timer fades out over its last seconds.
const FADE_SECONDS: f64 = 8.0;

/// What ytfast uses from a song's `WEB_REMIX` player response.
#[derive(Clone, Debug, Default)]
pub(super) struct PlayerInfo {
    /// Integrated loudness in LKFS.
    pub loudness: Option<f64>,
    /// `playbackTracking.videostatsPlaybackUrl.baseUrl`, for history.
    pub tracking: Option<String>,
}

pub(super) fn player_info(player: &Value) -> PlayerInfo {
    let audio = parse::at(player, &["playerConfig", "audioConfig"]);
    let number = |key: &str| audio.and_then(|a| a.get(key)).and_then(Value::as_f64);
    // `loudnessDb` is relative to `loudnessTargetLkfs` (−7 on 2026-10-01).
    let loudness = number("trackAbsoluteLoudnessLkfs").or_else(|| {
        number("loudnessDb").map(|db| db + number("loudnessTargetLkfs").unwrap_or(-7.0))
    });
    let tracking = parse::at(
        player,
        &["playbackTracking", "videostatsPlaybackUrl", "baseUrl"],
    )
    .and_then(Value::as_str)
    .map(str::to_owned);
    PlayerInfo { loudness, tracking }
}

/// The gain that brings a song measured at `lkfs` to the target, to a
/// hundredth of a dB (what mpv is given and reports back).
fn level(lkfs: f64) -> f64 {
    ((TARGET_LKFS - lkfs).clamp(-24.0, MAX_BOOST_DB) * 100.0).round() / 100.0
}

impl super::Worker {
    /// A song's gain: 0 with levelling off, `None` while its loudness is unknown.
    pub(super) fn gain_for(&self, video_id: &str) -> Option<f64> {
        if !self.state.normalize {
            return Some(0.0);
        }
        self.players
            .get(video_id)
            .map(|p| p.loudness.map_or(0.0, level))
    }

    /// mpv's per-file options for a stream of `video_id`, and the gain in them.
    pub(super) fn file_options(
        &self,
        video_id: &str,
        stream: &Stream,
        start: Option<f64>,
    ) -> (Vec<(&'static str, String)>, Option<f64>) {
        let gain = self.gain_for(video_id);
        // Always set, so a gain changed while the song plays ends with it.
        let mut options = vec![("volume-gain", format!("{:.2}", gain.unwrap_or(0.0)))];
        if let Some(agent) = &stream.user_agent {
            options.push(("user-agent", agent.clone()));
        }
        if let Some(at) = start.filter(|s| *s >= 1.0) {
            options.push(("start", format!("{at:.2}")));
        }
        (options, gain)
    }

    /// Fetches a song's player response unless it is known.
    pub(super) fn fetch_player(&self, video_id: &str) {
        if self.players.contains_key(video_id) {
            return;
        }
        let client = self.client.clone();
        let tx = self.internal_tx.clone();
        let video_id = video_id.to_owned();
        tokio::spawn(async move {
            match client.player(&video_id).await {
                Ok(value) => {
                    let info = player_info(&value);
                    let _ = tx.send(Internal::Player { video_id, info });
                }
                Err(ApiError::Auth) => {
                    let _ = tx.send(Internal::AuthFailed);
                }
                Err(error) => log::debug!("no player response for {video_id}: {error}"),
            }
        });
    }

    pub(super) fn remember_player(&mut self, video_id: String, info: PlayerInfo) {
        if self.players.len() > 500 {
            self.players.clear();
        }
        self.players.insert(video_id, info);
    }

    /// A player response arrived; the current song takes its gain at once.
    pub(super) async fn player_arrived(&mut self, video_id: String, info: PlayerInfo) {
        self.remember_player(video_id.clone(), info);
        self.audition_gain(&video_id).await;
        if self.current_entry.is_some()
            && self.current().is_some_and(|t| t.video_id == video_id)
            && let Some(gain) = self.gain_for(&video_id)
            && self.state.gain != Some(gain)
        {
            self.apply_gain(gain).await;
        }
    }

    /// Sets the current song's gain while it plays.
    async fn apply_gain(&mut self, gain: f64) {
        if let Some(mpv) = &self.mpv {
            let _ = mpv.set("volume-gain", json!(gain)).await;
        }
        self.state.gain = Some(gain);
        self.emit(true);
        #[cfg(feature = "e2e")]
        self.probe_gain();
    }

    pub(super) async fn set_normalize(&mut self, on: bool) {
        self.state.normalize = on;
        self.emit(true);
        self.update_settings(|s| s.normalize = Some(on));
        let gain = self
            .current()
            .map(|t| t.video_id.clone())
            .and_then(|id| self.gain_for(&id));
        if self.current_entry.is_some()
            && let Some(gain) = gain
        {
            self.apply_gain(gain).await;
        }
        if let Some(id) = self.state.audition.as_ref().map(|a| a.video_id.clone()) {
            self.audition_gain(&id).await;
        }
        if self.appended.is_some() {
            self.drop_appended().await;
            self.prefetch();
        }
    }

    /// Changes settings.json, keeping what other parts of ytfast saved there.
    pub(super) fn update_settings(&self, change: impl FnOnce(&mut crate::settings::Settings)) {
        let mut settings = crate::settings::Settings::load(&self.paths);
        change(&mut settings);
        if let Err(error) = settings.save(&self.paths) {
            self.sink
                .send(Event::Error(format!("Couldn't save the setting: {error}")));
        }
    }

    // ---- equalizer ----

    /// Gives a newly started mpv the equalizer.
    pub(super) async fn apply_equalizer(&mut self, mpv: &Mpv) {
        let filter = self.state.equalizer.filter();
        match mpv.set("af", json!(filter)).await {
            Ok(()) => self.af = filter,
            Err(error) => log::warn!("couldn't set the equalizer: {error:#}"),
        }
        #[cfg(feature = "e2e")]
        self.probe_af();
    }

    pub(super) async fn set_equalizer(&mut self, equalizer: Equalizer) {
        let before = std::mem::replace(&mut self.state.equalizer, equalizer);
        self.emit(true);
        self.eq_stamp += 1;
        let decks = self.decks();
        if !decks.is_empty() {
            let now = &self.state.equalizer;
            if before.active() && now.active() && !self.af.is_empty() {
                // The same graph: change its bands in place, without a gap.
                for mpv in &decks {
                    for (i, (old, new)) in before.gains.iter().zip(now.gains).enumerate() {
                        if (old - new).abs() >= 0.05 {
                            let _ = mpv
                                .command(json!([
                                    "af-command",
                                    LABEL,
                                    "g",
                                    format!("{new:.1}"),
                                    format!("equalizer@b{i}")
                                ]))
                                .await;
                        }
                    }
                    if (before.preamp() - now.preamp()).abs() >= 0.05 {
                        let _ = mpv
                            .command(json!([
                                "af-command",
                                LABEL,
                                "volume",
                                format!("{:.1}dB", now.preamp()),
                                "volume@pre"
                            ]))
                            .await;
                    }
                }
            } else {
                let filter = now.filter();
                self.set_af(&decks, filter).await;
            }
        }
        let stamp = self.eq_stamp;
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            let _ = tx.send(Internal::EqualizerSettled { stamp });
        });
    }

    /// Edits stopped: `af` takes the bands too, and the choice is saved.
    pub(super) async fn equalizer_settled(&mut self, stamp: u64) {
        if stamp != self.eq_stamp {
            return;
        }
        let filter = self.state.equalizer.filter();
        if filter != self.af {
            let decks = self.decks();
            self.set_af(&decks, filter).await;
        }
        let equalizer = self.state.equalizer.clone();
        self.update_settings(|s| s.equalizer = equalizer);
        #[cfg(feature = "e2e")]
        self.probe_af();
    }

    /// Gives every deck the equalizer graph `filter`.
    async fn set_af(&mut self, decks: &[Arc<Mpv>], filter: String) {
        let mut applied = false;
        for mpv in decks {
            match mpv.set("af", json!(filter)).await {
                Ok(()) => applied = true,
                Err(error) => log::warn!("couldn't set the equalizer: {error:#}"),
            }
        }
        if applied {
            self.af = filter;
        }
    }

    // ---- sleep timer ----

    pub(super) fn sleeping_at_song_end(&self) -> bool {
        matches!(
            self.state.sleep,
            Some(SleepTimer {
                choice: Sleep::EndOfSong,
                ..
            })
        )
    }

    /// Repeat one loops the song in mpv, unless the timer waits for its end.
    pub(super) async fn apply_loop(&self) {
        let looping = self.state.repeat == Repeat::One && !self.sleeping_at_song_end();
        if let Some(mpv) = &self.mpv {
            let _ = mpv
                .set("loop-file", json!(if looping { "inf" } else { "no" }))
                .await;
        }
    }

    pub(super) async fn set_sleep(&mut self, choice: Option<Sleep>) {
        let was_song_end = self.sleeping_at_song_end();
        let stamp = self.sleep_stamp.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.sleep = choice.map(|choice| SleepTimer {
            choice,
            deadline: match choice {
                Sleep::Minutes(minutes) => {
                    Some(Instant::now() + Duration::from_secs(u64::from(minutes) * 60))
                }
                Sleep::EndOfSong => None,
            },
        });
        self.restore_fade().await;
        self.apply_loop().await;
        if self.sleeping_at_song_end() {
            // mpv stops at this song's end instead of moving on.
            self.drop_appended().await;
        } else if was_song_end && self.current_entry.is_some() {
            self.prefetch();
        }
        if let Some(Sleep::Minutes(minutes)) = choice {
            log::info!("sleep timer: {minutes} min");
            let tx = self.internal_tx.clone();
            let current = self.sleep_stamp.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    if current.load(Ordering::SeqCst) != stamp
                        || tx.send(Internal::SleepTick { stamp }).is_err()
                    {
                        return;
                    }
                }
            });
        }
        self.emit(true);
    }

    pub(super) async fn sleep_tick(&mut self, stamp: u64) {
        if stamp != self.sleep_stamp.load(Ordering::SeqCst) {
            return;
        }
        let Some(deadline) = self.state.sleep.and_then(|s| s.deadline) else {
            return;
        };
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_secs_f64();
        if left <= 0.0 {
            self.sleep_now().await;
        } else if left < FADE_SECONDS {
            self.fade_to(left / FADE_SECONDS).await;
        }
    }

    /// End of song: the fade follows the song's last seconds.
    pub(super) async fn song_end_fade(&mut self) {
        if !self.sleeping_at_song_end() || self.state.duration <= 0.0 {
            return;
        }
        let left = self.state.duration - self.state.position;
        if left < FADE_SECONDS {
            self.fade_to(left / FADE_SECONDS).await;
        } else if self.fade < 1.0 {
            // Seeked back out of the fade.
            self.restore_fade().await;
        }
    }

    async fn fade_to(&mut self, share: f64) {
        let share = share.clamp(0.0, 1.0);
        if (share - self.fade).abs() < 0.01 {
            return;
        }
        self.fade = share;
        self.apply_volumes().await;
        #[cfg(feature = "e2e")]
        crate::e2e::probe_push("sleep_fade", json!(self.state.volume * share));
    }

    pub(super) async fn restore_fade(&mut self) {
        if self.fade < 1.0 {
            self.fade = 1.0;
            self.apply_volumes().await;
        }
    }

    /// The timer ran out: pause, then put the volume back.
    async fn sleep_now(&mut self) {
        self.sleep_stamp.fetch_add(1, Ordering::SeqCst);
        self.state.sleep = None;
        self.finish_blend().await;
        if let (Some(mpv), false) = (&self.mpv, self.idle) {
            let _ = mpv.set("pause", json!(true)).await;
        }
        self.restore_fade().await;
        self.emit(true);
        log::info!("sleep timer: paused");
        #[cfg(feature = "e2e")]
        self.probe_sleep();
    }

    /// End of song: the song ended. The next one waits at its start, paused.
    pub(super) async fn sleep_after_song(&mut self) {
        self.sleep_stamp.fetch_add(1, Ordering::SeqCst);
        self.state.sleep = None;
        self.restore_fade().await;
        self.apply_loop().await;
        self.state.playing = false;
        self.state.loading = false;
        if let Some(pos) = self.pos {
            let next = if self.state.repeat == Repeat::One {
                Some(pos)
            } else if pos + 1 < self.queue.len() {
                Some(pos + 1)
            } else if self.state.repeat == Repeat::All {
                Some(0)
            } else {
                None
            };
            match next {
                Some(next) => self.park(next),
                None => self.state.position = self.state.duration,
            }
        }
        self.emit(true);
        self.save_session(true);
        log::info!("sleep timer: stopped at the end of the song");
        #[cfg(feature = "e2e")]
        self.probe_sleep();
    }

    /// Makes `pos` current without playing it; Play starts it.
    fn park(&mut self, pos: usize) {
        let Some(track) = self.track_at(pos).cloned() else {
            return;
        };
        self.generation += 1;
        self.pos = Some(pos);
        self.current_entry = None;
        self.appended = None;
        self.resume_at = None;
        self.state.index = Some(pos);
        self.state.position = 0.0;
        self.state.duration = track.duration.map(f64::from).unwrap_or(0.0);
        self.state.format = None;
        self.state.gain = None;
        self.state.lyrics = None;
        self.state.related = None;
        self.fetch_watch_info(&track.video_id);
    }

    // ---- E2E probes: what mpv actually has ----

    #[cfg(feature = "e2e")]
    pub(super) fn probe_gain(&self) {
        let (Some(mpv), Some(track)) = (self.mpv.clone(), self.current().cloned()) else {
            return;
        };
        let expected = self.state.gain;
        let normalize = self.state.normalize;
        let loudness = self.players.get(&track.video_id).and_then(|p| p.loudness);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let applied = mpv.get("volume-gain").await.ok();
            crate::e2e::probe_push(
                "gains",
                json!({
                    "video_id": track.video_id,
                    "normalize": normalize,
                    "loudness_lkfs": loudness,
                    "gain_db": expected,
                    "mpv_volume_gain": applied,
                }),
            );
        });
    }

    #[cfg(feature = "e2e")]
    fn probe_af(&self) {
        let Some(mpv) = self.mpv.clone() else { return };
        let sent = self.af.clone();
        let equalizer = self.state.equalizer.clone();
        tokio::spawn(async move {
            let applied = mpv.get("af").await.ok();
            crate::e2e::probe(
                "af",
                json!({
                    "preset": equalizer.preset.label(),
                    "enabled": equalizer.enabled,
                    "sent": sent,
                    "mpv_af": applied,
                }),
            );
        });
    }

    #[cfg(feature = "e2e")]
    fn probe_sleep(&self) {
        let Some(mpv) = self.mpv.clone() else { return };
        let volume = self.state.volume;
        tokio::spawn(async move {
            let pause = mpv.get("pause").await.ok();
            let mpv_volume = mpv.get("volume").await.ok();
            crate::e2e::probe(
                "sleep_stopped",
                json!({"mpv_pause": pause, "mpv_volume": mpv_volume, "volume": volume}),
            );
        });
    }
}
