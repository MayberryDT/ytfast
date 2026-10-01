//! The tray icon: while the window is closed and Music plays on, an item in
//! the bar's tray (a StatusNotifierItem, which Omarchy's bar hosts) shows the
//! song, brings the window back, plays or pauses, and quits. While a window is
//! open the item stays registered but Passive, which trays hide.
//!
//! Like MPRIS, it is fed from the backend's playback state ([`Now`]) and not
//! the window. ksni serves it over zbus on async-io, the reactor AccessKit's
//! and MPRIS's zbus already use.

use std::time::{Duration, Instant};

use ksni::menu::StandardItem;
use ksni::{Category, MenuItem, Orientation, Status, ToolTip, TrayMethods};
use tokio::sync::watch;

use crate::backend::Command;
use crate::desktop::{Now, Remote, Request};

/// A symbolic icon, which trays tint with the theme's foreground.
const ICON: &str = "audio-x-generic-symbolic";
/// Volume change per wheel notch (120 units), in percent.
const VOLUME_STEP: f64 = 5.0;
/// Scrolls this close together build on each other rather than on the
/// volume the backend last reported, which lags behind them.
const SCROLL_RUN: Duration = Duration::from_millis(800);

/// What the tray shows.
#[derive(Clone, Debug, Default, PartialEq)]
struct Shown {
    window_open: bool,
    /// "Title · Artists" of the song playing or paused.
    song: Option<String>,
    playing: bool,
}

impl Shown {
    fn of(now: &Now, window_open: bool) -> Self {
        Self {
            window_open,
            song: now.track().map(|t| {
                let artists = t.artist_line();
                if artists.is_empty() {
                    t.title.clone()
                } else {
                    format!("{} · {artists}", t.title)
                }
            }),
            playing: now.playback.playing,
        }
    }
}

struct Tray {
    remote: Remote,
    shown: Shown,
    /// The volume the last scroll set, and when.
    scrolled: Option<(f64, Instant)>,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "ytfast".into()
    }

    fn title(&self) -> String {
        "Music".into()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn status(&self) -> Status {
        if self.shown.window_open {
            Status::Passive
        } else {
            Status::Active
        }
    }

    fn icon_name(&self) -> String {
        ICON.into()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: self.shown.song.clone().unwrap_or_else(|| "Music".into()),
            ..ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.remote.request(Request::Show);
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        self.remote.toggle();
    }

    fn scroll(&mut self, delta: i32, orientation: Orientation) {
        if matches!(orientation, Orientation::Horizontal) {
            return;
        }
        let base = match self.scrolled {
            Some((volume, at)) if at.elapsed() < SCROLL_RUN => volume,
            _ => self.remote.now().playback.volume,
        };
        let volume = (base + f64::from(delta) / 120.0 * VOLUME_STEP).clamp(0.0, 100.0);
        self.scrolled = Some((volume, Instant::now()));
        self.remote.command(Command::Volume(volume));
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let song = self.shown.song.is_some();
        vec![
            StandardItem {
                label: if self.shown.playing { "Pause" } else { "Play" }.into(),
                enabled: song,
                activate: Box::new(|tray: &mut Self| tray.remote.toggle()),
                ..StandardItem::default()
            }
            .into(),
            StandardItem {
                label: "Next".into(),
                enabled: song,
                activate: Box::new(|tray: &mut Self| tray.remote.command(Command::Next)),
                ..StandardItem::default()
            }
            .into(),
            StandardItem {
                label: "Previous".into(),
                enabled: song,
                activate: Box::new(|tray: &mut Self| tray.remote.command(Command::Previous)),
                ..StandardItem::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Show Music".into(),
                activate: Box::new(|tray: &mut Self| tray.remote.request(Request::Show)),
                ..StandardItem::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| tray.remote.request(Request::Quit)),
                ..StandardItem::default()
            }
            .into(),
        ]
    }
}

/// Registers the tray item and keeps it current until the runtime ends.
/// Without a tray host on the session bus, ytfast runs on without it.
pub fn start(
    runtime: &tokio::runtime::Handle,
    remote: Remote,
    mut now: watch::Receiver<Now>,
    mut window_open: watch::Receiver<bool>,
) {
    runtime.spawn(async move {
        let shown = Shown::of(&now.borrow_and_update(), *window_open.borrow_and_update());
        let tray = Tray {
            remote,
            shown: shown.clone(),
            scrolled: None,
        };
        let handle = match tray.spawn().await {
            Ok(handle) => handle,
            Err(error) => {
                log::warn!("no tray icon: {error}");
                return;
            }
        };
        log::info!("tray: registered");
        let mut current = shown;
        loop {
            tokio::select! {
                changed = now.changed() => if changed.is_err() { break },
                changed = window_open.changed() => if changed.is_err() { break },
            }
            let next = Shown::of(&now.borrow_and_update(), *window_open.borrow_and_update());
            // Playback reports arrive several times a second; the tray only
            // hears about what it shows.
            if next != current {
                current = next.clone();
                if handle.update(|tray| tray.shown = next).await.is_none() {
                    break;
                }
            }
        }
    });
}
