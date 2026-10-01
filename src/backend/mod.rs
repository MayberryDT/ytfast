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

mod account;
mod pages;
mod playback;
mod session;

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
    /// A change to the signed-in account; `op` stamps the answer. On success
    /// the `refresh` pages are asked for again once YouTube Music shows it.
    AccountEdit {
        op: u64,
        edit: crate::account::Edit,
        refresh: Vec<Target>,
    },
    /// Fetch a song's rating on the account (answered with `Event::Likes`).
    LikeStatus(String),
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
    /// The answer to `Command::AccountEdit` number `op`.
    AccountEdited {
        op: u64,
        result: Result<crate::account::Done, crate::account::Failure>,
    },
    /// Ratings as YouTube Music returned them: (video id, rating).
    Likes(Vec<(String, crate::model::LikeStatus)>),
    /// Pages to fetch again after an account change.
    AccountRefresh(Vec<Target>),
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
            Command::AccountEdit { op, edit, refresh } => self.account_edit(op, edit, refresh),
            Command::LikeStatus(video_id) => self.like_status(video_id),
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
