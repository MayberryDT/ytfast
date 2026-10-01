//! What the desktop integration shares: the playing state as the backend
//! last sent it ([`Now`], fed from every `Event::Queue`/`Event::Playback`),
//! the requests that only the interface can carry out ([`Request`]), and
//! [`Remote`], which MPRIS and the command line use to drive the app.
//! Transport goes straight to the backend, so it works with no window open.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use tokio::sync::watch;

use crate::backend::{Command, Event};
use crate::model::{Playback, Track};
use crate::paths::Paths;
use crate::single_instance::Message;

/// The queue (in play order) and playback state, as the interface has them.
#[derive(Clone, Debug, Default)]
pub struct Now {
    pub queue: Vec<Track>,
    pub playback: Playback,
}

impl Now {
    pub fn track(&self) -> Option<&Track> {
        self.playback.index.and_then(|i| self.queue.get(i))
    }
}

/// Mirrors an event bound for the interface into `now`.
pub(crate) fn observe(now: &watch::Sender<Now>, event: &Event) {
    match event {
        Event::Queue(queue) => now.send_modify(|n| n.queue.clone_from(queue)),
        Event::Playback(playback) => now.send_modify(|n| n.playback.clone_from(playback)),
        _ => {}
    }
}

/// What the interface does for the desktop: window and account matters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Bring the window back (relaunch, `ytfast show`, MPRIS Raise).
    Show,
    /// Quit for real (`ytfast quit`, MPRIS Quit).
    Quit,
    /// Open a YouTube Music or YouTube link.
    Open(String),
    /// Like or unlike the playing song (`ytfast like`).
    Like,
    ReloadThemes,
}

/// Flags the interface sets and the backend's desktop tasks read.
#[derive(Debug, Default)]
pub struct Flags {
    /// A ytfast window has the keyboard focus.
    pub focused: AtomicBool,
    /// Settings: "Show a notification when the song changes".
    pub notifications: AtomicBool,
}

/// Drives the app from outside the window: MPRIS and the command line.
#[derive(Clone)]
pub struct Remote {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    now: watch::Receiver<Now>,
    requests: mpsc::Sender<Request>,
    waker: fastframe_shell::Waker,
}

impl Remote {
    pub fn new(
        commands: tokio::sync::mpsc::UnboundedSender<Command>,
        now: watch::Receiver<Now>,
        requests: mpsc::Sender<Request>,
        waker: fastframe_shell::Waker,
    ) -> Self {
        Self {
            commands,
            now,
            requests,
            waker,
        }
    }

    pub fn now(&self) -> watch::Ref<'_, Now> {
        self.now.borrow()
    }

    pub fn command(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Hands a request to the interface; a window repaints at once, and the
    /// background loop picks it up within its tick.
    pub fn request(&self, request: Request) {
        if self.requests.send(request).is_ok() {
            self.waker.wake();
        }
    }

    pub fn toggle(&self) {
        if self.now().track().is_some() {
            self.command(Command::TogglePause);
        }
    }

    pub fn play(&self) {
        let (has_track, playing) = {
            let now = self.now();
            (now.track().is_some(), now.playback.playing)
        };
        if has_track && !playing {
            self.command(Command::TogglePause);
        }
    }

    pub fn pause(&self) {
        if self.now().playback.playing {
            self.command(Command::TogglePause);
        }
    }

    /// Carries out a command-line message.
    pub fn deliver(&self, message: Message) {
        match message {
            Message::Show => self.request(Request::Show),
            Message::ReloadThemes => self.request(Request::ReloadThemes),
            Message::Toggle => self.toggle(),
            Message::Play => self.play(),
            Message::Pause => self.pause(),
            Message::Next => self.command(Command::Next),
            Message::Previous => self.command(Command::Previous),
            Message::Like => self.request(Request::Like),
            Message::Quit => self.request(Request::Quit),
            Message::Open(link) => self.request(Request::Open(link)),
        }
    }
}

/// The cover file `url` is cached in, downloading it first when the
/// interface hasn't (the window may be closed). The same cache the
/// interface's cover loader uses.
pub(crate) async fn cached_cover(
    http: &reqwest::Client,
    paths: &Paths,
    url: &str,
) -> Option<PathBuf> {
    let file = paths.cover_file(url);
    if tokio::fs::metadata(&file).await.is_ok_and(|m| m.len() > 0) {
        return Some(file);
    }
    let response = http
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .ok()?;
    let bytes = response.bytes().await.ok()?;
    let target = file.clone();
    tokio::task::spawn_blocking(move || crate::paths::write_atomic(&target, &bytes))
        .await
        .ok()?
        .ok()?;
    Some(file)
}
