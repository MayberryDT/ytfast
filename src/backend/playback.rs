use super::*;

impl super::Worker {
    /// A play request replaces the queue: results of earlier ones no longer apply.
    pub(super) fn new_epoch(&mut self) -> u64 {
        self.epoch += 1;
        self.extending = false;
        self.advance_pending = false;
        self.waiting_for_network = false;
        self.epoch
    }

    pub(super) fn set_queue(&mut self, tracks: Vec<Track>, start: usize) {
        self.queue = tracks;
        self.build_order(start.min(self.queue.len().saturating_sub(1)));
        self.send_queue();
    }

    /// Rebuilds the play order with `current` (a queue index) at the
    /// current position; shuffled order puts it first.
    pub(super) fn build_order(&mut self, current: usize) {
        if self.queue.is_empty() {
            self.order.clear();
            self.pos = None;
            return;
        }
        if self.state.shuffle {
            let mut rest: Vec<usize> = (0..self.queue.len()).filter(|&i| i != current).collect();
            fastrand::shuffle(&mut rest);
            self.order = std::iter::once(current).chain(rest).collect();
            self.pos = Some(0);
        } else {
            self.order = (0..self.queue.len()).collect();
            self.pos = Some(current);
        }
    }

    pub(super) fn send_queue(&self) {
        let ordered = self.order.iter().map(|&i| self.queue[i].clone()).collect();
        self.sink.send(Event::Queue(ordered));
    }

    fn track_at(&self, pos: usize) -> Option<&Track> {
        self.order.get(pos).and_then(|&i| self.queue.get(i))
    }

    /// Starts the track at play-order position `pos`.
    pub(super) async fn start(&mut self, pos: usize) {
        let Some(track) = self.track_at(pos).cloned() else {
            return;
        };
        self.generation += 1;
        self.pos = Some(pos);
        self.current_entry = None;
        self.appended = None;
        self.retried = false;
        self.reported = false;
        self.waiting_for_network = false;
        self.state.index = Some(pos);
        self.state.loading = true;
        self.state.position = 0.0;
        self.state.duration = track.duration.map(f64::from).unwrap_or(0.0);
        self.state.format = None;
        self.state.lyrics = None;
        self.state.related = None;
        self.emit(true);
        if let Some(mpv) = &self.mpv {
            // Stop the previous song at once; the new one follows when resolved.
            let _ = mpv.command(json!(["stop"])).await;
        }
        self.resolve_current(&track.video_id);
        self.fetch_watch_info(&track.video_id);
        self.maybe_extend();
    }

    fn resolve_current(&self, video_id: &str) {
        let generation = self.generation;
        let resolver = self.resolver.clone();
        let tx = self.internal_tx.clone();
        let id = video_id.to_owned();
        tokio::spawn(async move {
            let stream = resolver.resolve(&id).await;
            let _ = tx.send(Internal::Started { generation, stream });
        });
    }

    fn fetch_watch_info(&self, video_id: &str) {
        let generation = self.generation;
        let client = self.client.clone();
        let tx = self.internal_tx.clone();
        let target = Target::Watch {
            video_id: Some(video_id.to_owned()),
            playlist_id: None,
            params: None,
        };
        tokio::spawn(async move {
            if let Ok(value) = client.next(&target).await {
                let _ = tx.send(Internal::Watch {
                    generation,
                    info: parse::watch_next(&value),
                });
            }
        });
    }

    /// Resolves the next tracks and appends the next one to mpv's playlist
    /// so the change is gapless.
    pub(super) fn prefetch(&self) {
        let Some(pos) = self.pos else { return };
        let upcoming: Vec<(usize, String)> = (pos + 1..=pos + 2)
            .filter_map(|p| self.track_at(p).map(|t| (p, t.video_id.clone())))
            .collect();
        if upcoming.is_empty() {
            return;
        }
        let generation = self.generation;
        let resolver = self.resolver.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            for (p, video_id) in upcoming {
                let stream = match resolver.resolve(&video_id).await {
                    Ok(stream) => stream,
                    Err(error) => {
                        log::warn!("resolving an upcoming track failed: {error:#}");
                        continue;
                    }
                };
                if p == pos + 1 {
                    let _ = tx.send(Internal::NextReady {
                        generation,
                        pos: p,
                        video_id,
                        stream,
                    });
                }
            }
        });
    }

    async fn ensure_mpv(&mut self) -> Option<Arc<Mpv>> {
        if self.mpv.is_none() {
            match Mpv::spawn(
                &self.paths.runtime.join("mpv.sock"),
                self.state.volume,
                self.mpv_tx.clone(),
            )
            .await
            {
                Ok(mpv) => {
                    if self.state.repeat == Repeat::One {
                        let _ = mpv.set("loop-file", json!("inf")).await;
                    }
                    self.mpv = Some(mpv);
                }
                Err(error) => {
                    self.sink.send(Event::Error(format!(
                        "Couldn't start the audio player: {error:#}"
                    )));
                    self.state.loading = false;
                    self.emit(true);
                    return None;
                }
            }
        }
        self.mpv.clone()
    }

    pub(super) async fn next(&mut self, automatic: bool) {
        let Some(pos) = self.pos else { return };
        if pos + 1 < self.order.len() {
            if let (Some(appended), Some(mpv), false) = (&self.appended, &self.mpv, self.idle)
                && appended.pos == pos + 1
            {
                let _ = mpv.command(json!(["playlist-next", "force"])).await;
                return;
            }
            self.start(pos + 1).await;
        } else if self.state.repeat == Repeat::All && !self.order.is_empty() {
            self.start(0).await;
        } else if self.state.autoplay {
            // Continue with radio for the last track; play it as soon as it arrives.
            self.state.loading = true;
            self.emit(true);
            if self.extending {
                self.advance_pending = true;
            } else {
                self.extend(true);
            }
        } else if automatic {
            self.state.playing = false;
            self.state.loading = false;
            self.emit(true);
        }
    }

    pub(super) async fn seek(&mut self, seconds: f64) {
        if let Some(mpv) = &self.mpv {
            let _ = mpv
                .command(json!(["seek", seconds.max(0.0), "absolute"]))
                .await;
            self.state.position = seconds;
            self.emit(true);
        }
    }

    pub(super) async fn drop_appended(&mut self) {
        if self.appended.take().is_some()
            && let Some(mpv) = &self.mpv
        {
            let _ = mpv.command(json!(["playlist-remove", 1])).await;
        }
    }

    /// Autoplay: when the last track in the queue is playing, fetch a radio
    /// to follow it.
    pub(super) fn maybe_extend(&mut self) {
        let Some(pos) = self.pos else { return };
        if self.state.autoplay && !self.extending && pos + 1 >= self.order.len() {
            self.extend(false);
        }
    }

    /// Fetches YouTube Music's radio for the last track of the queue.
    fn extend(&mut self, then_play: bool) {
        let Some(last) = self.order.last().and_then(|&i| self.queue.get(i)) else {
            return;
        };
        self.extending = true;
        let target = Target::Watch {
            video_id: Some(last.video_id.clone()),
            playlist_id: Some(format!("RDAMVM{}", last.video_id)),
            params: Some("wAEB".into()),
        };
        let known: std::collections::HashSet<String> =
            self.queue.iter().map(|t| t.video_id.clone()).collect();
        let client = self.client.clone();
        let tx = self.internal_tx.clone();
        let epoch = self.epoch;
        tokio::spawn(async move {
            let tracks = match client.next(&target).await {
                Ok(value) => parse::watch_next(&value)
                    .tracks
                    .into_iter()
                    .filter(|t| !known.contains(&t.video_id))
                    .collect(),
                Err(error) => {
                    log::warn!("autoplay radio failed: {error}");
                    Vec::new()
                }
            };
            let _ = tx.send(Internal::Extended {
                epoch,
                tracks,
                then_play,
                autoplay: true,
            });
        });
    }

    pub(super) async fn internal(&mut self, message: Internal) {
        match message {
            Internal::Connected(account) => self.sink.send(Event::Account(account)),
            Internal::AuthFailed => {
                if self.client.signed_in()
                    && self
                        .last_connect
                        .is_none_or(|t| t.elapsed() > Duration::from_secs(60))
                {
                    self.connect();
                }
            }
            Internal::Started { generation, stream } => {
                if generation != self.generation {
                    return;
                }
                let Some(track) = self.pos.and_then(|p| self.track_at(p)).cloned() else {
                    return;
                };
                match stream {
                    Ok(stream) => {
                        let Some(mpv) = self.ensure_mpv().await else {
                            return;
                        };
                        // `replace` empties mpv's playlist, including any track queued behind.
                        self.appended = None;
                        match mpv
                            .load(&stream.url, "replace", stream.user_agent.as_deref())
                            .await
                        {
                            Ok(entry) => {
                                self.current_entry = Some(entry);
                                let _ = mpv.set("pause", json!(false)).await;
                                self.state.format = Some(resolver::describe(stream.itag));
                                self.emit(true);
                                self.prefetch();
                            }
                            Err(error) => self.fail(&track, &format!("{error:#}")).await,
                        }
                    }
                    Err(error) => self.fail(&track, &format!("{error:#}")).await,
                }
            }
            Internal::NextReady {
                generation,
                pos,
                video_id,
                stream,
            } => {
                // The play order may have changed (shuffle) while it resolved.
                let still_next = self.pos.map(|p| p + 1) == Some(pos)
                    && self.track_at(pos).is_some_and(|t| t.video_id == video_id);
                if generation != self.generation
                    || !still_next
                    || self.appended.is_some()
                    || self.current_entry.is_none()
                {
                    return;
                }
                if let Some(mpv) = &self.mpv
                    && let Ok(entry) = mpv
                        .load(&stream.url, "append", stream.user_agent.as_deref())
                        .await
                {
                    self.appended = Some(Appended {
                        pos,
                        itag: stream.itag,
                        entry,
                    });
                    self.emit(true);
                }
            }
            Internal::Watch { generation, info } => {
                if generation != self.generation {
                    return;
                }
                self.state.lyrics = info.lyrics;
                self.state.related = info.related;
                if let Some(like) = info.like {
                    self.sink.send(Event::Likes(vec![like]));
                }
                self.emit(true);
            }
            Internal::Queue { epoch, result } => {
                if epoch != self.epoch {
                    return;
                }
                match result {
                    Ok(info) => {
                        let start = info.current;
                        self.set_queue(info.tracks, start);
                        if let Some(pos) = self.pos {
                            self.start(pos).await;
                        }
                    }
                    Err(error) => {
                        self.state.loading = false;
                        self.emit(true);
                        self.sink
                            .send(Event::Error(format!("Couldn't start playback: {error}")));
                    }
                }
            }
            Internal::Extended {
                epoch,
                tracks,
                then_play,
                autoplay,
            } => {
                if epoch != self.epoch {
                    return;
                }
                if autoplay {
                    self.extending = false;
                }
                let play_now = then_play || (autoplay && std::mem::take(&mut self.advance_pending));
                let first_new = self.order.len();
                let known: std::collections::HashSet<String> =
                    self.queue.iter().map(|t| t.video_id.clone()).collect();
                for track in tracks.into_iter().filter(|t| !known.contains(&t.video_id)) {
                    self.order.push(self.queue.len());
                    self.queue.push(track);
                }
                self.send_queue();
                if play_now && first_new < self.order.len() {
                    self.start(first_new).await;
                } else if play_now {
                    self.state.loading = false;
                    self.state.playing = false;
                    self.emit(true);
                } else if self.appended.is_none() {
                    self.prefetch();
                }
            }
            Internal::Failed {
                generation,
                title,
                error,
                online,
            } => {
                if generation != self.generation {
                    return;
                }
                if online {
                    self.sink.send(Event::Error(format!(
                        "Couldn't play “{title}”, skipped it. {error}"
                    )));
                    self.next(true).await;
                } else {
                    // Offline: don't skip through the queue; play this song when the connection returns.
                    self.waiting_for_network = true;
                    self.state.loading = true;
                    self.emit(true);
                    self.sink.send(Event::Error(format!(
                        "No connection. “{title}” will play when it's back."
                    )));
                    let client = self.client.clone();
                    let tx = self.internal_tx.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            if client.reachable().await {
                                let _ = tx.send(Internal::Online { generation });
                                return;
                            }
                            if tx.is_closed() {
                                return;
                            }
                        }
                    });
                }
            }
            Internal::Online { generation } => {
                if generation == self.generation
                    && self.waiting_for_network
                    && let Some(pos) = self.pos
                {
                    self.start(pos).await;
                }
            }
        }
    }

    /// A track failed: retry once with a freshly resolved stream; then skip,
    /// unless YouTube is unreachable, in which case wait for the connection.
    async fn fail(&mut self, track: &Track, error: &str) {
        log::warn!("playback failed: {error}");
        if !self.retried {
            self.retried = true;
            self.resolver.forget(&track.video_id);
            self.resolve_current(&track.video_id);
            return;
        }
        let generation = self.generation;
        let client = self.client.clone();
        let tx = self.internal_tx.clone();
        let (title, error) = (track.title.clone(), error.to_owned());
        tokio::spawn(async move {
            let online = client.reachable().await;
            let _ = tx.send(Internal::Failed {
                generation,
                title,
                error,
                online,
            });
        });
    }

    pub(super) async fn mpv_event(&mut self, event: MpvEvent) {
        match event {
            MpvEvent::Property { name, data } => match name.as_str() {
                "time-pos" => {
                    // Before the current file loads, positions belong to the previous one.
                    let (Some(position), Some(_)) = (data.as_f64(), self.current_entry) else {
                        return;
                    };
                    self.state.position = position;
                    if !self.reported && position >= 10.0 {
                        self.reported = true;
                        if let Some(track) = self.pos.and_then(|p| self.track_at(p)) {
                            let client = self.client.clone();
                            let id = track.video_id.clone();
                            tokio::spawn(async move {
                                if let Err(error) = client.report_play(&id).await {
                                    log::warn!("reporting a play failed: {error}");
                                }
                            });
                        }
                    }
                    self.emit(false);
                }
                "duration" => {
                    if let (Some(duration), Some(_)) = (data.as_f64(), self.current_entry) {
                        self.state.duration = duration;
                        self.emit(true);
                    }
                }
                "pause" => {
                    self.paused = data.as_bool() == Some(true);
                    self.state.playing = !self.paused && !self.idle && self.pos.is_some();
                    self.emit(true);
                }
                "paused-for-cache" | "seeking" => {
                    if !self.waiting_for_network {
                        self.state.loading = data.as_bool() == Some(true);
                        self.emit(true);
                    }
                }
                "idle-active" => {
                    self.idle = data.as_bool() == Some(true);
                    if !self.idle {
                        self.state.loading = false;
                    }
                    self.state.playing = !self.paused && !self.idle && self.pos.is_some();
                    self.emit(true);
                    // Safety net: mpv ran out of tracks although one was thought
                    // to be queued behind the current one. Move on ourselves.
                    if self.idle
                        && !self.state.loading
                        && self.appended.is_some()
                        && self.pos.is_some()
                    {
                        log::warn!("mpv went idle with a track thought queued; advancing");
                        self.appended = None;
                        self.next(true).await;
                    }
                }
                "playlist-pos" => {
                    // mpv moved on to the track appended behind the current one.
                    if data.as_i64() == Some(1)
                        && let Some(appended) = self.appended.take()
                    {
                        self.current_entry = Some(appended.entry);
                        if let Some(mpv) = &self.mpv {
                            let _ = mpv.command(json!(["playlist-remove", 0])).await;
                        }
                        self.generation += 1;
                        self.pos = Some(appended.pos);
                        self.retried = false;
                        self.reported = false;
                        let track = self.track_at(appended.pos).cloned();
                        self.state.index = Some(appended.pos);
                        self.state.position = 0.0;
                        self.state.duration = track
                            .as_ref()
                            .and_then(|t| t.duration)
                            .map(f64::from)
                            .unwrap_or(0.0);
                        self.state.format = Some(resolver::describe(appended.itag));
                        self.state.lyrics = None;
                        self.state.related = None;
                        self.emit(true);
                        if let Some(track) = track {
                            self.fetch_watch_info(&track.video_id);
                        }
                        self.prefetch();
                        self.maybe_extend();
                    }
                }
                _ => {}
            },
            MpvEvent::EndFile {
                reason,
                error,
                entry,
            } => {
                log::debug!(
                    "mpv end-file {entry} {reason} {error:?}; current {:?}, queued {:?}",
                    self.current_entry,
                    self.appended.as_ref().map(|a| a.entry)
                );
                // Events of replaced or queued entries are not about the current track.
                if Some(entry) != self.current_entry {
                    return;
                }
                match reason.as_str() {
                    "eof" if self.appended.is_none() && self.state.repeat != Repeat::One => {
                        self.next(true).await
                    }
                    "error" => {
                        // Keep mpv from moving on to the queued track: this one is
                        // retried or skipped first.
                        self.drop_appended().await;
                        self.current_entry = None;
                        if let Some(track) = self.pos.and_then(|p| self.track_at(p)).cloned() {
                            let error = error.unwrap_or_else(|| "the stream failed".into());
                            self.fail(&track, &error).await;
                        }
                    }
                    _ => {}
                }
            }
            MpvEvent::StartFile { entry } => log::debug!("mpv start-file {entry}"),
            MpvEvent::Died => {
                self.mpv = None;
                self.appended = None;
                self.current_entry = None;
                self.idle = true;
                let was_playing = self.state.playing;
                self.state.playing = false;
                self.state.loading = false;
                self.emit(true);
                let recent = self
                    .last_death
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(30));
                self.last_death = Some(Instant::now());
                match (was_playing, recent, self.pos) {
                    (true, false, Some(pos)) => {
                        self.sink.send(Event::Error(
                            "The audio player stopped unexpectedly; restarting the song.".into(),
                        ));
                        self.start(pos).await;
                    }
                    (true, true, _) => {
                        self.sink.send(Event::Error(
                            "The audio player keeps stopping. Press Play to try again.".into(),
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
}
