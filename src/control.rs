//! Control (docs/SPEC.md § Control): what the keyboard map, the context
//! menus and Play anything ask of the app beyond a plain backend command:
//! Forward after Back, mute, relative seeks and volume steps, an album's or
//! playlist's songs fetched through the page machinery before they are
//! queued, and saving the queue as a playlist.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::account::{AccountAction, Dialog};
use crate::app::{App, View};
use crate::backend::Command;
use crate::model::{Target, Track};

/// The most songs one page gives Play next or Add to queue (as many as
/// playing a playlist loads).
const MOST: usize = 500;
/// A page that hasn't come within this long is given up on, with a note.
const PATIENCE: Duration = Duration::from_secs(30);

/// What to do with an album's, playlist's or artist's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FromPage {
    Play,
    /// Play next, keeping the page's order.
    Next,
    /// Add to queue, keeping the page's order.
    Queue,
    Shuffle,
    Radio,
}

#[derive(Clone, Debug)]
pub enum ControlAction {
    /// Alt+→: the view Back left.
    Forward,
    /// Seek this many seconds from where playback is.
    SeekBy(f64),
    /// Turn the volume up (or down) by this many points.
    VolumeBy(f64),
    /// Mute, or go back to the volume before.
    Mute,
    /// Show or hide the keyboard shortcuts.
    Help(bool),
    /// Open or close Play anything.
    PlayAnything(bool),
    /// Play, queue, shuffle or start the radio of a page's music, loading
    /// the page (and the rest of a long playlist) first when needed.
    FromPage { page: Target, how: FromPage },
    /// Up next → Save: the new-playlist dialog, holding the queue.
    SaveQueue,
    /// Put a link on the clipboard (a menu's Copy link).
    CopyLink(String),
}

struct Pending {
    page: Target,
    how: FromPage,
    since: Instant,
}

#[derive(Default)]
pub struct ControlState {
    /// Views that Back left, newest last; emptied when another view opens.
    pub forward: Vec<View>,
    /// The volume before Mute.
    unmuted: Option<f64>,
    /// The keyboard shortcuts are shown.
    pub help: bool,
    pending: Vec<Pending>,
    pub play_anything: crate::palette::PlayAnything,
    /// The last link put on the clipboard.
    pub copied: Option<String>,
}

impl App {
    pub(crate) fn control_action(&mut self, ctx: &egui::Context, action: ControlAction) {
        match action {
            ControlAction::Forward => self.go_forward(),
            ControlAction::SeekBy(seconds) => {
                let duration = self.playback.duration;
                if self.current_track().is_some() && duration > 0.0 {
                    let to = (self.position_now() + seconds).clamp(0.0, (duration - 1.0).max(0.0));
                    self.backend.send(Command::Seek(to));
                }
            }
            ControlAction::VolumeBy(step) => {
                let volume = (self.playback.volume + step).clamp(0.0, 100.0);
                self.control.unmuted = None;
                self.set_volume(volume);
            }
            ControlAction::Mute => {
                if self.playback.volume > 0.0 {
                    self.control.unmuted = Some(self.playback.volume);
                    self.set_volume(0.0);
                } else {
                    let back = self.control.unmuted.take().unwrap_or(70.0);
                    self.set_volume(back);
                }
            }
            ControlAction::Help(on) => self.control.help = on,
            ControlAction::PlayAnything(on) => self.control.play_anything.set_open(on),
            ControlAction::FromPage { page, how } => {
                self.ensure_page(page.clone(), false);
                let pending = Pending {
                    page,
                    how,
                    since: Instant::now(),
                };
                // A page already here answers in this frame.
                if !self.run_pending(&pending) {
                    self.control.pending.push(pending);
                }
            }
            ControlAction::SaveQueue => {
                // Each song once, in queue order.
                let mut seen = std::collections::HashSet::new();
                let tracks: Vec<Track> = self
                    .queue
                    .iter()
                    .filter(|t| seen.insert(t.video_id.clone()))
                    .cloned()
                    .collect();
                if !tracks.is_empty() {
                    self.account_action(AccountAction::Dialog(Some(Dialog::NewPlaylist {
                        title: format!("Queue · {}", today()),
                        description: String::new(),
                        tracks,
                    })));
                }
            }
            ControlAction::CopyLink(link) => {
                ctx.copy_text(link.clone());
                self.control.copied = Some(link);
            }
        }
    }

    fn set_volume(&mut self, volume: f64) {
        self.playback.volume = volume;
        self.backend.send(Command::Volume(volume));
    }

    fn go_forward(&mut self) {
        if let Some(view) = self.control.forward.pop() {
            let previous = std::mem::replace(&mut self.view, view);
            self.history.push(previous);
            self.now_playing = false;
            self.scroll_to_top = true;
            self.ensure_page(self.view.target(), false);
        }
    }

    /// Every frame: carries on with pages being fetched for a menu or
    /// Play anything, and asks YouTube Music for Play anything's results.
    pub(crate) fn control_frame(&mut self, ctx: &egui::Context) {
        if !self.control.pending.is_empty() {
            for pending in std::mem::take(&mut self.control.pending) {
                if self.run_pending(&pending) {
                    continue;
                }
                if pending.since.elapsed() > PATIENCE {
                    self.push_error("That took too long to load. Try again.".into());
                } else {
                    self.control.pending.push(pending);
                }
            }
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        self.play_anything_frame(ctx);
    }

    /// Does what `pending` asks once its page is here; `false` while it
    /// still has to wait (for the page or the rest of a long playlist).
    fn run_pending(&mut self, pending: &Pending) -> bool {
        let key = pending.page.key();
        let Some(state) = self.pages.get_mut(&key) else {
            return false;
        };
        let Some(page) = &state.page else {
            if let (false, Some(error)) = (state.loading, &state.error) {
                let error = format!("Couldn't load that: {error}");
                self.push_error(error);
                return true;
            }
            return false;
        };
        // A saved copy may be out of date: wait for the fresh page.
        if state.loading {
            return false;
        }
        let header = page.header.as_ref();
        let target = match pending.how {
            FromPage::Radio => header.and_then(|h| h.radio.clone().or_else(|| playlist_radio(h))),
            FromPage::Play => header.and_then(|h| h.play.clone()),
            FromPage::Shuffle => header.and_then(|h| h.shuffle.clone()),
            FromPage::Next | FromPage::Queue => None,
        };
        if let Some(target) = target {
            self.backend.send(Command::PlayTarget(target));
            return true;
        }
        if pending.how == FromPage::Radio {
            self.push_error("There's no radio for this.".into());
            return true;
        }
        // A playlist's own list when it has one: YouTube Music's Suggestions
        // come after it on the page and aren't in the playlist.
        let shelves: Vec<(usize, &crate::model::Shelf)> = match crate::account::entries(page) {
            Some(own) => page
                .shelves
                .iter()
                .enumerate()
                .filter(|(_, s)| std::ptr::eq(*s, own))
                .collect(),
            None => page.shelves.iter().enumerate().collect(),
        };
        let mut tracks: Vec<Track> = shelves
            .iter()
            .flat_map(|(_, s)| &s.items)
            .filter_map(|i| i.track.clone())
            .collect();
        // A long playlist comes in parts: fetch the rest first.
        let rest = shelves
            .iter()
            .rev()
            .find_map(|(i, s)| Some((*i, s.continuation.clone()?)));
        if let Some((shelf, token)) = rest
            && tracks.len() < MOST
        {
            if state.more_loading.insert(Some(shelf)) {
                self.backend.send(Command::More {
                    key,
                    token,
                    search: false,
                    shelf: Some(shelf),
                });
            }
            return false;
        }
        tracks.truncate(MOST);
        if tracks.is_empty() {
            self.push_error("There are no songs to play here.".into());
            return true;
        }
        self.backend.send(match pending.how {
            FromPage::Next => Command::PlayNext(tracks),
            FromPage::Queue => Command::AddToQueue(tracks),
            FromPage::Shuffle => {
                fastrand::shuffle(&mut tracks);
                Command::PlayTracks { tracks, start: 0 }
            }
            FromPage::Play | FromPage::Radio => Command::PlayTracks { tracks, start: 0 },
        });
        true
    }
}

/// Today's date as "1 Oct 2026" (UTC: the standard library knows no time
/// zones, so just after midnight it may still say yesterday).
fn today() -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{day} {} {year}", MONTHS[(month - 1) as usize])
}

/// An album's or playlist's radio when its header offers none: YouTube
/// Music's `RDAMPL` mix of its playlist (a mix already is one).
fn playlist_radio(header: &crate::model::Header) -> Option<Target> {
    let list = match &header.play {
        Some(Target::Watch {
            playlist_id: Some(list),
            ..
        }) => list.clone(),
        _ => header.library.as_ref()?.playlist_id.clone(),
    };
    let playlist_id = if list.starts_with("RD") {
        list
    } else {
        format!("RDAMPL{list}")
    };
    Some(Target::Watch {
        video_id: None,
        playlist_id: Some(playlist_id),
        params: None,
    })
}
