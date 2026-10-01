//! The interface's handle to a tokio runtime that does all I/O: InnerTube,
//! the browser session, yt-dlp and mpv. The two sides talk only through
//! [`Command`] (interface → backend) and [`Event`] (backend → interface);
//! every event wakes the window.
//!
//! Playback state belongs to the worker. Results of asynchronous work carry
//! the stamp they were started under, so a late answer never acts on newer
//! state: `generation` changes with the current track, `epoch` with the
//! queue (each play request), and mpv's playlist entry ids tell the current
//! file's events from those of replaced or queued ones.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::mpsc;

use crate::innertube::{ApiError, Client, Stream};
use crate::model::{Account, Lyrics, Page, Playback, Repeat, Target, Track, WatchNext};
use crate::mpv::{Mpv, MpvEvent};
use crate::parse::{self, More};
use crate::paths::Paths;
use crate::resolver::{self, Resolver};

pub enum Command {
    /// Open a page: cached copy first, then fresh. `seq` identifies the
    /// request; only the newest one's answer is used.
    Page {
        target: Target,
        seq: u64,
    },
    /// Load the next part of a page (`shelf: None`) or of one of its shelves.
    More {
        key: String,
        token: String,
        search: bool,
        shelf: Option<usize>,
    },
    Suggest(String),
    Lyrics(String),
    /// Play `tracks`, starting at `start`, as the queue.
    PlayTracks {
        tracks: Vec<Track>,
        start: usize,
    },
    /// Play a song radio, playlist, album or mix through watch-next.
    PlayTarget(Target),
    TogglePause,
    Next,
    Previous,
    Seek(f64),
    Volume(f64),
    ToggleShuffle,
    CycleRepeat,
    Autoplay(bool),
    /// Jump to a queue position (play order).
    JumpTo(usize),
    Reconnect,
    /// Resolve a song the pointer rests on, if nothing else is resolving.
    Prepare(String),
    /// Use this browser profile's YouTube session from now on, and reconnect.
    UseProfile(String),
}

pub enum Event {
    Account(Account),
    Page {
        key: String,
        seq: u64,
        result: Result<Box<Page>, String>,
        cached: bool,
    },
    /// The answer to a continuation `token`.
    More {
        key: String,
        shelf: Option<usize>,
        token: String,
        result: Result<More, String>,
    },
    Suggestions {
        input: String,
        items: Vec<String>,
    },
    Lyrics {
        id: String,
        result: Result<Option<Lyrics>, String>,
    },
    /// The queue in play order.
    Queue(Vec<Track>),
    Playback(Playback),
    /// A readable error for the error strip.
    Error(String),
    /// The browser profiles signed in to YouTube, and the one in use.
    Profiles {
        list: Vec<crate::auth::Profile>,
        current: Option<String>,
    },
}

#[derive(Clone)]
struct Sink {
    tx: std::sync::mpsc::Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Sink {
    fn send(&self, event: Event) {
        let _ = self.tx.send(event);
        (self.wake)();
    }
}

pub struct Backend {
    commands: mpsc::UnboundedSender<Command>,
    pub events: std::sync::mpsc::Receiver<Event>,
    pub http: reqwest::Client,
    pub runtime: tokio::runtime::Handle,
    _runtime: tokio::runtime::Runtime,
}

impl Backend {
    pub fn start(paths: Paths, wake: impl Fn() + Send + Sync + 'static) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("ytfast-io")
            .build()?;
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (tx, events) = std::sync::mpsc::channel();
        let sink = Sink {
            tx,
            wake: Arc::new(wake),
        };
        let client = Arc::new(Client::new());
        let http = client.http().clone();
        let resolver = Arc::new(Resolver::new(paths.runtime.clone()));
        runtime.spawn(Worker::new(client, resolver, paths, sink).run(command_rx));
        Ok(Self {
            commands,
            events,
            http,
            runtime: runtime.handle().clone(),
            _runtime: runtime,
        })
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

enum Internal {
    Connected(Account),
    AuthFailed,
    Started {
        generation: u64,
        stream: anyhow::Result<Stream>,
    },
    NextReady {
        generation: u64,
        pos: usize,
        video_id: String,
        stream: Stream,
    },
    Watch {
        generation: u64,
        info: WatchNext,
    },
    Queue {
        epoch: u64,
        result: Result<WatchNext, String>,
    },
    /// More queue: a long playlist's next page, or the autoplay radio.
    Extended {
        epoch: u64,
        tracks: Vec<Track>,
        then_play: bool,
        autoplay: bool,
    },
    /// A track failed twice; `online` says whether YouTube is reachable.
    Failed {
        generation: u64,
        title: String,
        error: String,
        online: bool,
    },
    /// The connection is back after a failure while offline.
    Online {
        generation: u64,
    },
}

/// The track queued in mpv behind the current one.
#[derive(Clone)]
struct Appended {
    /// Its play-order position.
    pos: usize,
    itag: u32,
    /// mpv's playlist entry id.
    entry: i64,
}

struct Worker {
    client: Arc<Client>,
    resolver: Arc<Resolver>,
    paths: Paths,
    sink: Sink,
    internal_tx: mpsc::UnboundedSender<Internal>,
    internal_rx: Option<mpsc::UnboundedReceiver<Internal>>,
    mpv_tx: mpsc::UnboundedSender<MpvEvent>,
    mpv_rx: Option<mpsc::UnboundedReceiver<MpvEvent>>,
    mpv: Option<Arc<Mpv>>,
    last_connect: Option<Instant>,
    last_death: Option<Instant>,

    queue: Vec<Track>,
    /// Play order: indices into `queue`.
    order: Vec<usize>,
    /// Position in `order` of the current track.
    pos: Option<usize>,
    appended: Option<Appended>,
    /// mpv's playlist entry id of the current track.
    current_entry: Option<i64>,
    /// Bumped whenever the current track changes.
    generation: u64,
    /// Bumped whenever a play request replaces the queue.
    epoch: u64,
    retried: bool,
    reported: bool,
    /// An autoplay radio fetch for the current epoch is in flight.
    extending: bool,
    /// Next was asked for while the radio was still loading.
    advance_pending: bool,
    /// A track failed while offline; it plays when the connection returns.
    waiting_for_network: bool,
    state: Playback,
    last_emit: Instant,
    /// mpv's `pause` and `idle-active`: playing means neither.
    paused: bool,
    idle: bool,
}

impl Worker {
    fn new(client: Arc<Client>, resolver: Arc<Resolver>, paths: Paths, sink: Sink) -> Self {
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
        let (mpv_tx, mpv_rx) = mpsc::unbounded_channel();
        Self {
            client,
            resolver,
            paths,
            sink,
            internal_tx,
            internal_rx: Some(internal_rx),
            mpv_tx,
            mpv_rx: Some(mpv_rx),
            mpv: None,
            last_connect: None,
            last_death: None,
            queue: Vec::new(),
            order: Vec::new(),
            pos: None,
            appended: None,
            current_entry: None,
            generation: 0,
            epoch: 0,
            retried: false,
            reported: false,
            extending: false,
            advance_pending: false,
            waiting_for_network: false,
            state: Playback {
                volume: 100.0,
                autoplay: true,
                ..Playback::default()
            },
            last_emit: Instant::now(),
            paused: false,
            idle: true,
        }
    }

    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let mut internal = self.internal_rx.take().expect("internal receiver");
        let mut mpv_events = self.mpv_rx.take().expect("mpv receiver");
        self.connect();
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.command(command).await,
                    None => break,
                },
                Some(message) = internal.recv() => self.internal(message).await,
                Some(event) = mpv_events.recv() => self.mpv_event(event).await,
            }
        }
    }

    // ---- session ----

    fn connect(&mut self) {
        self.last_connect = Some(Instant::now());
        self.sink.send(Event::Account(Account::Checking));
        let client = self.client.clone();
        let resolver = self.resolver.clone();
        let paths = self.paths.clone();
        let tx = self.internal_tx.clone();
        let sink = self.sink.clone();
        tokio::spawn(async move {
            let scratch = paths.runtime.clone();
            let preferred = crate::settings::Settings::load(&paths).browser_profile;
            let loaded = tokio::task::spawn_blocking(move || {
                let session = crate::auth::load(&scratch, preferred.as_deref());
                (session, crate::auth::profiles(&scratch))
            })
            .await
            .map(|(session, profiles)| {
                let current = session.as_ref().ok().map(|s| s.profile.clone());
                sink.send(Event::Profiles {
                    list: profiles,
                    current,
                });
                session
            });
            let session = match loaded {
                Ok(Ok(session)) => session,
                Ok(Err(error)) => {
                    client.set_session(None);
                    resolver.set_cookie_file(None);
                    let _ = tx.send(Internal::Connected(Account::SignedOut {
                        reason: format!("{error:#}"),
                    }));
                    return;
                }
                Err(error) => {
                    let _ = tx.send(Internal::Connected(Account::SignedOut {
                        reason: error.to_string(),
                    }));
                    return;
                }
            };
            let source = session.source.clone();
            let cookie_file = paths.cookie_file();
            match session.write_netscape(&cookie_file) {
                Ok(()) => resolver.set_cookie_file(Some(cookie_file)),
                Err(error) => log::warn!("could not write the cookie file: {error:#}"),
            }
            client.set_session(Some(session));
            let account = match client.account().await {
                Ok(value) => match parse::account(&value) {
                    Some((name, photo)) => Account::SignedIn {
                        name,
                        photo,
                        source,
                    },
                    None => {
                        client.set_session(None);
                        resolver.set_cookie_file(None);
                        Account::SignedOut {
                            reason: format!("{source} isn't signed in to YouTube Music"),
                        }
                    }
                },
                Err(ApiError::Auth) => {
                    client.set_session(None);
                    resolver.set_cookie_file(None);
                    Account::SignedOut {
                        reason: format!("The YouTube session in {source} has expired"),
                    }
                }
                Err(error) => Account::Unverified {
                    reason: error.to_string(),
                },
            };
            let _ = tx.send(Internal::Connected(account));
        });
    }

    // ---- commands ----

    async fn command(&mut self, command: Command) {
        match command {
            Command::Page { target, seq } => self.load_page(target, seq),
            Command::More {
                key,
                token,
                search,
                shelf,
            } => {
                let client = self.client.clone();
                let sink = self.sink.clone();
                let internal = self.internal_tx.clone();
                tokio::spawn(async move {
                    let result = if search {
                        client.search_continuation(&token).await
                    } else {
                        client.continuation(&token).await
                    };
                    if matches!(result, Err(ApiError::Auth)) {
                        let _ = internal.send(Internal::AuthFailed);
                    }
                    let result = result.map(|v| parse::more(&v)).map_err(|e| e.to_string());
                    sink.send(Event::More {
                        key,
                        shelf,
                        token,
                        result,
                    });
                });
            }
            Command::Suggest(input) => {
                let client = self.client.clone();
                let sink = self.sink.clone();
                tokio::spawn(async move {
                    if let Ok(value) = client.suggestions(&input).await {
                        sink.send(Event::Suggestions {
                            items: parse::suggestions(&value),
                            input,
                        });
                    }
                });
            }
            Command::Lyrics(id) => {
                let client = self.client.clone();
                let sink = self.sink.clone();
                tokio::spawn(async move {
                    let result = client
                        .browse(&id, None)
                        .await
                        .map(|v| parse::lyrics(&v))
                        .map_err(|e| e.to_string());
                    sink.send(Event::Lyrics { id, result });
                });
            }
            Command::PlayTracks { tracks, start } => {
                self.new_epoch();
                self.set_queue(tracks, start);
                if let Some(pos) = self.pos {
                    self.start(pos).await;
                }
            }
            Command::PlayTarget(target) => {
                let epoch = self.new_epoch();
                self.state.loading = true;
                self.emit(true);
                let client = self.client.clone();
                let tx = self.internal_tx.clone();
                tokio::spawn(async move {
                    let result = client.next(&target).await.map(|v| parse::watch_next(&v));
                    if matches!(result, Err(ApiError::Auth)) {
                        let _ = tx.send(Internal::AuthFailed);
                    }
                    let result = result.map_err(|e| e.to_string()).and_then(|info| {
                        if info.tracks.is_empty() {
                            Err("Nothing to play here".to_owned())
                        } else {
                            Ok(info)
                        }
                    });
                    let mut token = result
                        .as_ref()
                        .ok()
                        .and_then(|info| info.continuation.clone());
                    let mut total = result.as_ref().map(|info| info.tracks.len()).unwrap_or(0);
                    let _ = tx.send(Internal::Queue { epoch, result });
                    // Long playlists and albums: fetch the rest of the queue.
                    while let Some(t) = token.take().filter(|_| total < 500) {
                        let Ok(value) = client.next_continuation(&t).await else {
                            break;
                        };
                        let more = parse::watch_next(&value);
                        if more.tracks.is_empty() {
                            break;
                        }
                        total += more.tracks.len();
                        token = more.continuation.clone();
                        let extended = Internal::Extended {
                            epoch,
                            tracks: more.tracks,
                            then_play: false,
                            autoplay: false,
                        };
                        if tx.send(extended).is_err() {
                            break;
                        }
                    }
                });
            }
            Command::TogglePause => {
                if self.state.loading {
                    // Resolving: nothing to pause yet.
                } else if let (Some(mpv), false) = (&self.mpv, self.idle) {
                    let _ = mpv.set("pause", json!(self.state.playing)).await;
                } else if let Some(pos) = self.pos {
                    // Nothing loaded (the queue ended, or mpv restarted): play again.
                    self.start(pos).await;
                }
            }
            Command::Next => self.next(false).await,
            Command::Previous => {
                if self.state.position > 3.0 || self.pos == Some(0) {
                    self.seek(0.0).await;
                } else if let Some(pos) = self.pos {
                    self.start(pos - 1).await;
                }
            }
            Command::Seek(seconds) => self.seek(seconds).await,
            Command::Volume(volume) => {
                self.state.volume = volume.clamp(0.0, 100.0);
                if let Some(mpv) = &self.mpv {
                    let _ = mpv.set("volume", json!(self.state.volume)).await;
                }
                self.emit(true);
            }
            Command::ToggleShuffle => {
                self.state.shuffle = !self.state.shuffle;
                if let Some(pos) = self.pos {
                    let current = self.order[pos];
                    self.build_order(current);
                    self.drop_appended().await;
                    self.send_queue();
                    self.prefetch();
                }
                self.emit(true);
            }
            Command::CycleRepeat => {
                self.state.repeat = match self.state.repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                };
                if let Some(mpv) = &self.mpv {
                    let _ = mpv
                        .set(
                            "loop-file",
                            json!(if self.state.repeat == Repeat::One {
                                "inf"
                            } else {
                                "no"
                            }),
                        )
                        .await;
                }
                self.emit(true);
            }
            Command::Autoplay(on) => {
                self.state.autoplay = on;
                self.emit(true);
                self.maybe_extend();
            }
            Command::JumpTo(pos) => {
                if pos < self.order.len() {
                    self.start(pos).await;
                }
            }
            Command::Reconnect => self.connect(),
            Command::UseProfile(profile) => {
                let settings = crate::settings::Settings {
                    browser_profile: Some(profile),
                };
                if let Err(error) = settings.save(&self.paths) {
                    self.sink.send(Event::Error(format!(
                        "Couldn't save the account choice: {error}"
                    )));
                }
                self.connect();
            }
            Command::Prepare(video_id) => {
                let resolver = self.resolver.clone();
                tokio::spawn(async move { resolver.prepare(&video_id).await });
            }
        }
    }

    fn load_page(&self, target: Target, seq: u64) {
        let key = target.key();
        let client = self.client.clone();
        let sink = self.sink.clone();
        let path = self.paths.page_file(&key);
        let internal = self.internal_tx.clone();
        tokio::spawn(async move {
            if let Ok(bytes) = tokio::fs::read(&path).await
                && let Ok(page) = serde_json::from_slice::<Page>(&bytes)
            {
                sink.send(Event::Page {
                    key: key.clone(),
                    seq,
                    result: Ok(Box::new(page)),
                    cached: true,
                });
            }
            let result = match &target {
                Target::Browse { id, params } => client.browse(id, params.as_deref()).await,
                Target::Search { query, params } => client.search(query, params.as_deref()).await,
                Target::Watch { .. } => Err(ApiError::Invalid("not a page".into())),
            };
            match result {
                Ok(value) => {
                    let page = parse::page(&value);
                    if let Ok(bytes) = serde_json::to_vec(&page) {
                        let _ = crate::paths::write_atomic(&path, &bytes);
                    }
                    sink.send(Event::Page {
                        key,
                        seq,
                        result: Ok(Box::new(page)),
                        cached: false,
                    });
                }
                Err(error) => {
                    if matches!(error, ApiError::Auth) {
                        let _ = internal.send(Internal::AuthFailed);
                    }
                    sink.send(Event::Page {
                        key,
                        seq,
                        result: Err(error.to_string()),
                        cached: false,
                    });
                }
            }
        });
    }

    // ---- queue ----

    /// A play request replaces the queue: results of earlier ones no longer apply.
    fn new_epoch(&mut self) -> u64 {
        self.epoch += 1;
        self.extending = false;
        self.advance_pending = false;
        self.waiting_for_network = false;
        self.epoch
    }

    fn set_queue(&mut self, tracks: Vec<Track>, start: usize) {
        self.queue = tracks;
        self.build_order(start.min(self.queue.len().saturating_sub(1)));
        self.send_queue();
    }

    /// Rebuilds the play order with `current` (a queue index) at the
    /// current position; shuffled order puts it first.
    fn build_order(&mut self, current: usize) {
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

    fn send_queue(&self) {
        let ordered = self.order.iter().map(|&i| self.queue[i].clone()).collect();
        self.sink.send(Event::Queue(ordered));
    }

    fn track_at(&self, pos: usize) -> Option<&Track> {
        self.order.get(pos).and_then(|&i| self.queue.get(i))
    }

    // ---- playback ----

    /// Starts the track at play-order position `pos`.
    async fn start(&mut self, pos: usize) {
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
    fn prefetch(&self) {
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

    async fn next(&mut self, automatic: bool) {
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

    async fn seek(&mut self, seconds: f64) {
        if let Some(mpv) = &self.mpv {
            let _ = mpv
                .command(json!(["seek", seconds.max(0.0), "absolute"]))
                .await;
            self.state.position = seconds;
            self.emit(true);
        }
    }

    async fn drop_appended(&mut self) {
        if self.appended.take().is_some()
            && let Some(mpv) = &self.mpv
        {
            let _ = mpv.command(json!(["playlist-remove", 1])).await;
        }
    }

    /// Autoplay: when the last track in the queue is playing, fetch a radio
    /// to follow it.
    fn maybe_extend(&mut self) {
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

    // ---- results ----

    async fn internal(&mut self, message: Internal) {
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

    async fn mpv_event(&mut self, event: MpvEvent) {
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

    /// Sends the playback state; position-only updates at most four times a second.
    fn emit(&mut self, always: bool) {
        if !always && self.last_emit.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_emit = Instant::now();
        self.state.next_ready = self.appended.is_some();
        self.sink.send(Event::Playback(self.state.clone()));
    }
}
