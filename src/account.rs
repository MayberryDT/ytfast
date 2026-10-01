//! Changes to the signed-in account: likes and dislikes, saving albums and
//! playlists to the library, subscriptions, and the account's own playlists
//! (docs/SPEC.md § Journeys → Account; § Decisions → Write-back).
//!
//! Every change shows in the frame of the input: the app marks the new state
//! (or edits the cached page), then the backend asks YouTube Music. If YouTube
//! Music refuses, the change is rolled back and a plain message says so. A
//! few seconds after a change succeeds, the pages it affects are fetched
//! again; YouTube Music takes up to ~3 s to show some changes, so for a while
//! the app's own marks win over what a fetched page says.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app::{App, LibraryTab, View};
use crate::backend::Command;
use crate::model::{
    Account, Item, ItemKind, LibraryToggle, LikeStatus, Page, Run, Shelf, ShelfStyle, Subscription,
    Target, Track,
};

/// How long the app's own mark outlives what fetched pages say.
const TRUST_LOCAL: Duration = Duration::from_secs(15);

/// A write the backend makes to the account.
#[derive(Clone, Debug)]
pub enum Edit {
    Rate {
        video_id: String,
        status: LikeStatus,
    },
    /// Save an album (its audio playlist) or a playlist to the library, or remove it.
    Save {
        playlist_id: String,
        save: bool,
    },
    Subscribe {
        channel_id: String,
        subscribe: bool,
    },
    Create {
        title: String,
        description: String,
        video_ids: Vec<String>,
    },
    Add {
        playlist_id: String,
        video_ids: Vec<String>,
    },
    Remove {
        playlist_id: String,
        video_id: String,
        set_video_id: String,
    },
    /// Move an entry before `before` (the end when `None`).
    Move {
        playlist_id: String,
        set_video_id: String,
        before: Option<String>,
    },
    Details {
        playlist_id: String,
        title: Option<String>,
        description: Option<String>,
    },
    Delete {
        playlist_id: String,
    },
}

/// What a successful edit returned.
#[derive(Clone, Debug)]
pub enum Done {
    Ok,
    /// A new playlist's id.
    Created(String),
    /// Songs added to a playlist: (video id, entry id).
    Added(Vec<(String, String)>),
}

/// Why an edit didn't happen.
#[derive(Clone, Debug)]
pub enum Failure {
    /// YouTube Music answered and said no (detail for Copy).
    Refused(String),
    AlreadyInPlaylist,
    Offline,
    SignedOut,
}

/// What the interface asks for.
#[derive(Clone, Debug)]
pub enum AccountAction {
    /// Give `track` this rating (`Indifferent` removes a like or dislike).
    Rate {
        track: Track,
        status: LikeStatus,
    },
    Save {
        playlist_id: String,
        title: String,
        save: bool,
    },
    Subscribe {
        channel_id: String,
        name: String,
        subscribe: bool,
    },
    Create {
        title: String,
        description: String,
        tracks: Vec<Track>,
    },
    Add {
        playlist_id: String,
        tracks: Vec<Track>,
    },
    Remove {
        playlist_id: String,
        set_video_id: String,
    },
    /// Move the entry `set_video_id` to where the entry `onto` is.
    Move {
        playlist_id: String,
        set_video_id: String,
        onto: String,
    },
    Details {
        playlist_id: String,
        title: String,
        description: String,
    },
    Delete {
        playlist_id: String,
    },
    Dialog(Option<Dialog>),
}

/// A dialog over the window.
#[derive(Clone, Debug)]
pub enum Dialog {
    /// Name and describe a new playlist, holding `tracks` (may be empty).
    NewPlaylist {
        title: String,
        description: String,
        tracks: Vec<Track>,
    },
    EditPlaylist {
        playlist_id: String,
        title: String,
        description: String,
    },
    DeletePlaylist {
        playlist_id: String,
        title: String,
    },
    /// Choose one of the account's playlists for `tracks`.
    AddToPlaylist {
        tracks: Vec<Track>,
        filter: String,
        selected: usize,
    },
}

/// The account's state as the app knows it, over what pages say.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    pub likes: HashMap<String, LikeStatus>,
    /// Library state by playlist id (an album's audio playlist id).
    pub saved: HashMap<String, bool>,
    /// Subscription state by channel id.
    pub subscribed: HashMap<String, bool>,
}

impl Marks {
    pub fn like(&self, track: &Track) -> LikeStatus {
        self.likes
            .get(&track.video_id)
            .copied()
            .or(track.like)
            .unwrap_or(LikeStatus::Indifferent)
    }

    pub fn saved(&self, toggle: &LibraryToggle) -> bool {
        self.saved
            .get(&toggle.playlist_id)
            .copied()
            .unwrap_or(toggle.saved)
    }

    pub fn subscribed(&self, subscription: &Subscription) -> bool {
        self.subscribed
            .get(&subscription.channel_id)
            .copied()
            .unwrap_or(subscription.subscribed)
    }
}

#[derive(Default)]
pub struct AccountState {
    /// Shared with the views each frame (cheap to hand out).
    pub marks: Arc<Marks>,
    pub dialog: Option<Dialog>,
    /// The newest ratings YouTube Music returned (watch-next), as fetched.
    pub fetched_likes: HashMap<String, LikeStatus>,
    /// The last playlist created here: (title, id).
    pub created: Option<(String, String)>,
    /// When each id was last changed here.
    changed: HashMap<String, Instant>,
    pending: HashMap<u64, Pending>,
    next_op: u64,
}

impl AccountState {
    /// Changes still waiting for YouTube Music's answer.
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    fn subjects_in_flight(&self) -> HashSet<&str> {
        self.pending.values().map(|p| p.subject.as_str()).collect()
    }

    /// Whether a fetched page may replace the app's mark for `id`.
    fn page_wins(&self, id: &str, busy: &HashSet<&str>) -> bool {
        !busy.contains(id)
            && self
                .changed
                .get(id)
                .is_none_or(|t| t.elapsed() > TRUST_LOCAL)
    }
}

struct Pending {
    /// The id the change is about.
    subject: String,
    /// "Couldn't like “Get Lucky”."
    failed: String,
    undo: Undo,
}

enum Undo {
    Like {
        video_id: String,
        set: LikeStatus,
        before: Option<LikeStatus>,
    },
    Saved {
        id: String,
        set: bool,
        before: Option<bool>,
    },
    Subscribed {
        id: String,
        set: bool,
        before: Option<bool>,
    },
    /// A placeholder card for a playlist being created.
    Created {
        title: String,
    },
    /// Rows appended to a playlist's pages, not yet confirmed.
    Added {
        playlist_id: String,
        video_ids: Vec<String>,
        title: String,
        playlist: String,
    },
    Removed {
        playlist_id: String,
        index: usize,
        item: Box<Item>,
    },
    Moved {
        playlist_id: String,
        from: usize,
        to: usize,
    },
    Details {
        playlist_id: String,
        title: String,
        description: Option<String>,
    },
    Deleted {
        index: usize,
        item: Box<Item>,
    },
}

fn quoted(title: &str) -> String {
    format!("“{title}”")
}

fn playlist_target(playlist_id: &str) -> Target {
    Target::browse(format!("VL{playlist_id}"))
}

fn library_target() -> Target {
    LibraryTab::Playlists.target()
}

/// A row for a song added to a playlist page before YouTube Music lists it.
fn row_for(track: &Track) -> Item {
    let mut subtitle = track.artists.clone();
    if let Some(album) = &track.album {
        if !subtitle.is_empty() {
            subtitle.push(Run {
                text: " • ".into(),
                target: None,
            });
        }
        subtitle.push(album.clone());
    }
    Item {
        kind: ItemKind::Song,
        title: track.title.clone(),
        subtitle,
        thumbnail: track.thumbnail.clone(),
        target: Some(Target::Watch {
            video_id: Some(track.video_id.clone()),
            playlist_id: None,
            params: None,
        }),
        play: None,
        track: Some(Track {
            set_video_id: None,
            ..track.clone()
        }),
        index: None,
        stripe: None,
        editable: None,
    }
}

/// Where the songs of a playlist page are: (shelf, item) for every song row.
fn song_rows(page: &Page) -> Vec<(usize, usize)> {
    page.shelves
        .iter()
        .enumerate()
        .flat_map(|(s, shelf)| {
            shelf
                .items
                .iter()
                .enumerate()
                .filter(|(_, i)| i.track.is_some())
                .map(move |(i, _)| (s, i))
        })
        .collect()
}

fn entry_of(item: &Item) -> Option<&str> {
    item.track.as_ref()?.set_video_id.as_deref()
}

fn is_card_of(item: &Item, playlist_id: &str) -> bool {
    item.editable.as_deref() == Some(playlist_id)
        || matches!(&item.target, Some(Target::Browse { id, .. }) if id.strip_prefix("VL") == Some(playlist_id))
}

impl App {
    fn signed_in(&self) -> bool {
        matches!(self.account, Account::SignedIn { .. })
    }

    fn marks_mut(&mut self) -> &mut Marks {
        Arc::make_mut(&mut self.account_state.marks)
    }

    /// The cached pages of a playlist (it may be open under more than one key).
    fn playlist_keys(&self, playlist_id: &str) -> Vec<String> {
        let key = playlist_target(playlist_id).key();
        self.pages
            .iter()
            .filter(|(k, s)| {
                **k == key
                    || s.page
                        .as_ref()
                        .and_then(|p| p.header.as_ref())
                        .is_some_and(|h| h.editable.as_deref() == Some(playlist_id))
            })
            .map(|(k, _)| k.clone())
            .collect()
    }

    fn playlist_pages(&mut self, playlist_id: &str) -> Vec<&mut Page> {
        let keys = self.playlist_keys(playlist_id);
        self.pages
            .iter_mut()
            .filter(|(k, _)| keys.contains(k))
            .filter_map(|(_, s)| s.page.as_mut())
            .collect()
    }

    fn library_page(&mut self) -> Option<&mut Page> {
        self.pages
            .get_mut(&library_target().key())
            .and_then(|s| s.page.as_mut())
    }

    /// The account's playlists that it can edit, from Library: (id, title).
    pub fn own_playlists(&self) -> Vec<(String, String)> {
        self.page_state(&library_target())
            .and_then(|s| s.page.as_ref())
            .map(|p| {
                p.shelves
                    .iter()
                    .flat_map(|s| &s.items)
                    .filter_map(|i| Some((i.editable.clone()?, i.title.clone())))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn playlist_title(&self, playlist_id: &str) -> String {
        let from_library = self
            .own_playlists()
            .into_iter()
            .find(|(id, _)| id == playlist_id)
            .map(|(_, t)| t);
        from_library
            .or_else(|| {
                self.playlist_keys(playlist_id).iter().find_map(|k| {
                    Some(
                        self.pages
                            .get(k)?
                            .page
                            .as_ref()?
                            .header
                            .as_ref()?
                            .title
                            .clone(),
                    )
                })
            })
            .unwrap_or_else(|| "the playlist".into())
    }

    fn send_edit(&mut self, edit: Edit, refresh: Vec<Target>, pending: Pending) {
        self.account_state.next_op += 1;
        let op = self.account_state.next_op;
        self.account_state
            .changed
            .insert(pending.subject.clone(), Instant::now());
        self.account_state.pending.insert(op, pending);
        self.backend
            .send(Command::AccountEdit { op, edit, refresh });
    }

    /// Likes the playing song, or removes its like (keyboard, command line, MPRIS).
    pub(crate) fn toggle_like_current(&mut self) {
        let Some(track) = self.playback.index.and_then(|i| self.queue.get(i)).cloned() else {
            return;
        };
        let status = match self.account_state.marks.like(&track) {
            LikeStatus::Like => LikeStatus::Indifferent,
            _ => LikeStatus::Like,
        };
        self.account_action(AccountAction::Rate { track, status });
    }

    pub(crate) fn account_action(&mut self, action: AccountAction) {
        if let AccountAction::Dialog(dialog) = action {
            self.account_state.dialog = dialog;
            return;
        }
        if !self.signed_in() {
            self.push_error(
                "Sign in to YouTube Music in your browser, then Reconnect, to change your library."
                    .into(),
            );
            return;
        }
        match action {
            AccountAction::Dialog(_) => {}
            AccountAction::Rate { track, status } => {
                let id = track.video_id.clone();
                let before = self.account_state.marks.likes.get(&id).copied();
                let was = self.account_state.marks.like(&track);
                self.marks_mut().likes.insert(id.clone(), status);
                let verb = match (status, was) {
                    (LikeStatus::Like, _) => "like".to_owned(),
                    (LikeStatus::Dislike, _) => "dislike".to_owned(),
                    (LikeStatus::Indifferent, LikeStatus::Dislike) => {
                        "remove the dislike from".to_owned()
                    }
                    (LikeStatus::Indifferent, _) => "unlike".to_owned(),
                };
                self.send_edit(
                    Edit::Rate {
                        video_id: id.clone(),
                        status,
                    },
                    vec![Target::browse("VLLM"), LibraryTab::Songs.target()],
                    Pending {
                        subject: id.clone(),
                        failed: format!("Couldn't {verb} {}.", quoted(&track.title)),
                        undo: Undo::Like {
                            video_id: id,
                            set: status,
                            before,
                        },
                    },
                );
            }
            AccountAction::Save {
                playlist_id,
                title,
                save,
            } => {
                let before = self.account_state.marks.saved.get(&playlist_id).copied();
                self.marks_mut().saved.insert(playlist_id.clone(), save);
                let failed = if save {
                    format!("Couldn't save {} to your library.", quoted(&title))
                } else {
                    format!("Couldn't remove {} from your library.", quoted(&title))
                };
                self.send_edit(
                    Edit::Save {
                        playlist_id: playlist_id.clone(),
                        save,
                    },
                    vec![LibraryTab::Albums.target(), LibraryTab::Playlists.target()],
                    Pending {
                        subject: playlist_id.clone(),
                        failed,
                        undo: Undo::Saved {
                            id: playlist_id,
                            set: save,
                            before,
                        },
                    },
                );
            }
            AccountAction::Subscribe {
                channel_id,
                name,
                subscribe,
            } => {
                let before = self
                    .account_state
                    .marks
                    .subscribed
                    .get(&channel_id)
                    .copied();
                self.marks_mut()
                    .subscribed
                    .insert(channel_id.clone(), subscribe);
                let failed = if subscribe {
                    format!("Couldn't subscribe to {}.", quoted(&name))
                } else {
                    format!("Couldn't unsubscribe from {}.", quoted(&name))
                };
                self.send_edit(
                    Edit::Subscribe {
                        channel_id: channel_id.clone(),
                        subscribe,
                    },
                    vec![
                        Target::browse("FEmusic_library_corpus_artists"),
                        LibraryTab::Artists.target(),
                    ],
                    Pending {
                        subject: channel_id.clone(),
                        failed,
                        undo: Undo::Subscribed {
                            id: channel_id,
                            set: subscribe,
                            before,
                        },
                    },
                );
            }
            AccountAction::Create {
                title,
                description,
                tracks,
            } => {
                let title = title.trim().to_owned();
                if title.is_empty() {
                    return;
                }
                if let Some(page) = self.library_page() {
                    let card = Item {
                        kind: ItemKind::Playlist,
                        title: title.clone(),
                        subtitle: vec![Run {
                            text: "Playlist".into(),
                            target: None,
                        }],
                        thumbnail: tracks.first().and_then(|t| t.thumbnail.clone()),
                        target: None,
                        play: None,
                        track: None,
                        index: None,
                        stripe: None,
                        editable: None,
                    };
                    match page.shelves.first_mut() {
                        // YouTube Music lists a new playlist first among the
                        // account's own, after the automatic ones.
                        Some(shelf) => {
                            let at = shelf
                                .items
                                .iter()
                                .position(|i| i.editable.is_some())
                                .unwrap_or(shelf.items.len().min(2));
                            shelf.items.insert(at, card);
                        }
                        None => page.shelves.push(Shelf {
                            title: String::new(),
                            strapline: None,
                            style: ShelfStyle::Grid,
                            items: vec![card],
                            more: None,
                            continuation: None,
                        }),
                    }
                }
                self.send_edit(
                    Edit::Create {
                        title: title.clone(),
                        description: description.trim().to_owned(),
                        video_ids: tracks.iter().map(|t| t.video_id.clone()).collect(),
                    },
                    vec![library_target()],
                    Pending {
                        subject: format!("new:{title}"),
                        failed: format!("Couldn't create {}.", quoted(&title)),
                        undo: Undo::Created { title },
                    },
                );
            }
            AccountAction::Add {
                playlist_id,
                tracks,
            } => {
                let playlist = self.playlist_title(&playlist_id);
                let Some(first) = tracks.first() else { return };
                let song = first.title.clone();
                // Already listed: say so without asking.
                let listed = self.playlist_keys(&playlist_id).iter().any(|k| {
                    self.pages
                        .get(k)
                        .and_then(|s| s.page.as_ref())
                        .is_some_and(|p| {
                            p.shelves.iter().flat_map(|s| &s.items).any(|i| {
                                i.track.as_ref().is_some_and(|t| {
                                    tracks.iter().any(|n| n.video_id == t.video_id)
                                })
                            })
                        })
                });
                if listed {
                    self.push_error(format!(
                        "{} is already in {}.",
                        quoted(&song),
                        quoted(&playlist)
                    ));
                    return;
                }
                for page in self.playlist_pages(&playlist_id) {
                    let rows: Vec<Item> = tracks.iter().map(row_for).collect();
                    page.message = None;
                    match page
                        .shelves
                        .iter_mut()
                        .rev()
                        .find(|s| s.style == ShelfStyle::List)
                    {
                        Some(shelf) => shelf.items.extend(rows),
                        None => page.shelves.push(Shelf {
                            title: String::new(),
                            strapline: None,
                            style: ShelfStyle::List,
                            items: rows,
                            more: None,
                            continuation: None,
                        }),
                    }
                }
                let video_ids: Vec<String> = tracks.iter().map(|t| t.video_id.clone()).collect();
                let what = if tracks.len() == 1 {
                    quoted(&song)
                } else {
                    format!("{} songs", tracks.len())
                };
                self.send_edit(
                    Edit::Add {
                        playlist_id: playlist_id.clone(),
                        video_ids: video_ids.clone(),
                    },
                    vec![playlist_target(&playlist_id), library_target()],
                    Pending {
                        subject: playlist_id.clone(),
                        failed: format!("Couldn't add {what} to {}.", quoted(&playlist)),
                        undo: Undo::Added {
                            playlist_id,
                            video_ids,
                            title: song,
                            playlist,
                        },
                    },
                );
            }
            AccountAction::Remove {
                playlist_id,
                set_video_id,
            } => {
                let playlist = self.playlist_title(&playlist_id);
                let mut removed = None;
                for page in self.playlist_pages(&playlist_id) {
                    for (s, i) in song_rows(page) {
                        if entry_of(&page.shelves[s].items[i]) == Some(set_video_id.as_str()) {
                            let item = page.shelves[s].items.remove(i);
                            removed.get_or_insert((i, item));
                            break;
                        }
                    }
                }
                let Some((index, item)) = removed else { return };
                let video_id = item
                    .track
                    .as_ref()
                    .map(|t| t.video_id.clone())
                    .unwrap_or_default();
                self.send_edit(
                    Edit::Remove {
                        playlist_id: playlist_id.clone(),
                        video_id,
                        set_video_id,
                    },
                    vec![playlist_target(&playlist_id), library_target()],
                    Pending {
                        subject: playlist_id.clone(),
                        failed: format!(
                            "Couldn't remove {} from {}.",
                            quoted(&item.title),
                            quoted(&playlist)
                        ),
                        undo: Undo::Removed {
                            playlist_id,
                            index,
                            item: Box::new(item),
                        },
                    },
                );
            }
            AccountAction::Move {
                playlist_id,
                set_video_id,
                onto,
            } => {
                if set_video_id == onto {
                    return;
                }
                let playlist = self.playlist_title(&playlist_id);
                // (from, to, successor, title) from the first page holding both.
                let mut moved: Option<(usize, usize, Option<String>, String)> = None;
                let mut not_ready = false;
                for page in self.playlist_pages(&playlist_id) {
                    let Some(shelf) = page.shelves.iter_mut().find(|s| {
                        s.items
                            .iter()
                            .any(|i| entry_of(i) == Some(set_video_id.as_str()))
                            && s.items.iter().any(|i| entry_of(i) == Some(onto.as_str()))
                    }) else {
                        continue;
                    };
                    let from = shelf
                        .items
                        .iter()
                        .position(|i| entry_of(i) == Some(set_video_id.as_str()))
                        .unwrap_or(0);
                    let to = shelf
                        .items
                        .iter()
                        .position(|i| entry_of(i) == Some(onto.as_str()))
                        .unwrap_or(0);
                    let item = shelf.items.remove(from);
                    let title = item.title.clone();
                    shelf.items.insert(to, item);
                    let successor: Option<Option<String>> = shelf
                        .items
                        .get(to + 1)
                        .map(|i| entry_of(i).map(str::to_owned));
                    if matches!(successor, Some(None)) {
                        // Just added, not yet listed by YouTube Music: put it back.
                        let item = shelf.items.remove(to);
                        shelf.items.insert(from, item);
                        not_ready = true;
                        break;
                    }
                    let before = successor.flatten();
                    if moved.is_none() {
                        moved = Some((from, to, before, title));
                    }
                }
                if not_ready {
                    self.push_error(format!(
                        "Couldn't move that song yet; {} is still saving. Try again in a moment.",
                        quoted(&playlist)
                    ));
                    return;
                }
                let Some((from, to, before, title)) = moved else {
                    return;
                };
                self.send_edit(
                    Edit::Move {
                        playlist_id: playlist_id.clone(),
                        set_video_id,
                        before,
                    },
                    vec![playlist_target(&playlist_id)],
                    Pending {
                        subject: playlist_id.clone(),
                        failed: format!(
                            "Couldn't move {} in {}.",
                            quoted(&title),
                            quoted(&playlist)
                        ),
                        undo: Undo::Moved {
                            playlist_id,
                            from,
                            to,
                        },
                    },
                );
            }
            AccountAction::Details {
                playlist_id,
                title,
                description,
            } => {
                let title = title.trim().to_owned();
                let description = description.trim().to_owned();
                if title.is_empty() {
                    return;
                }
                let old_title = self.playlist_title(&playlist_id);
                let mut old_description = None;
                for page in self.playlist_pages(&playlist_id) {
                    if let Some(h) = page.header.as_mut() {
                        old_description = h.description.clone();
                        h.title = title.clone();
                        h.description = Some(description.clone()).filter(|d| !d.is_empty());
                    }
                }
                if let Some(page) = self.library_page() {
                    for item in page.shelves.iter_mut().flat_map(|s| &mut s.items) {
                        if is_card_of(item, &playlist_id) {
                            item.title = title.clone();
                        }
                    }
                }
                let edit = Edit::Details {
                    playlist_id: playlist_id.clone(),
                    title: (title != old_title).then(|| title.clone()),
                    description: (old_description.as_deref().unwrap_or("") != description)
                        .then_some(description),
                };
                if matches!(
                    &edit,
                    Edit::Details {
                        title: None,
                        description: None,
                        ..
                    }
                ) {
                    return;
                }
                self.send_edit(
                    edit,
                    vec![playlist_target(&playlist_id), library_target()],
                    Pending {
                        subject: playlist_id.clone(),
                        failed: format!("Couldn't save the changes to {}.", quoted(&old_title)),
                        undo: Undo::Details {
                            playlist_id,
                            title: old_title,
                            description: old_description,
                        },
                    },
                );
            }
            AccountAction::Delete { playlist_id } => {
                let title = self.playlist_title(&playlist_id);
                let mut removed = None;
                if let Some(page) = self.library_page() {
                    for shelf in &mut page.shelves {
                        if let Some(i) =
                            shelf.items.iter().position(|i| is_card_of(i, &playlist_id))
                        {
                            removed = Some((i, shelf.items.remove(i)));
                            break;
                        }
                    }
                }
                // Leave the deleted playlist's page.
                let showing = matches!(&self.view, View::Page(t) if self.playlist_keys(&playlist_id).contains(&t.key()));
                if showing {
                    let back = self
                        .history
                        .pop()
                        .unwrap_or(View::Library(LibraryTab::Playlists));
                    self.view = back;
                    self.ensure_page(self.view.target(), false);
                }
                let (index, item) = removed.unwrap_or_else(|| {
                    (
                        0,
                        Item {
                            kind: ItemKind::Playlist,
                            title: title.clone(),
                            subtitle: Vec::new(),
                            thumbnail: None,
                            target: Some(playlist_target(&playlist_id)),
                            play: None,
                            track: None,
                            index: None,
                            stripe: None,
                            editable: Some(playlist_id.clone()),
                        },
                    )
                });
                self.send_edit(
                    Edit::Delete {
                        playlist_id: playlist_id.clone(),
                    },
                    vec![library_target()],
                    Pending {
                        subject: playlist_id,
                        failed: format!("Couldn't delete {}.", quoted(&title)),
                        undo: Undo::Deleted {
                            index,
                            item: Box::new(item),
                        },
                    },
                );
            }
        }
    }

    /// YouTube Music's answer to an edit.
    pub(crate) fn account_edited(&mut self, op: u64, result: Result<Done, Failure>) {
        let Some(pending) = self.account_state.pending.remove(&op) else {
            return;
        };
        match result {
            Ok(done) => self.confirm(pending, done),
            Err(failure) => {
                let message = match (&failure, &pending.undo) {
                    (
                        Failure::AlreadyInPlaylist,
                        Undo::Added {
                            title, playlist, ..
                        },
                    ) => {
                        format!("{} is already in {}.", quoted(title), quoted(playlist))
                    }
                    (Failure::Offline, _) => {
                        format!("{} Check the connection and try again.", pending.failed)
                    }
                    (Failure::SignedOut, _) => format!(
                        "{} YouTube Music signed you out; Reconnect and try again.",
                        pending.failed
                    ),
                    (Failure::Refused(detail), _) => {
                        log::warn!("account edit refused: {detail}");
                        format!("{} YouTube Music didn't accept it.", pending.failed)
                    }
                    (Failure::AlreadyInPlaylist, _) => {
                        format!("{} YouTube Music didn't accept it.", pending.failed)
                    }
                };
                // Let fetched pages speak for this id again.
                self.account_state.changed.remove(&pending.subject);
                self.roll_back(pending.undo);
                self.push_error(message);
            }
        }
    }

    fn confirm(&mut self, pending: Pending, done: Done) {
        match (pending.undo, done) {
            (Undo::Created { title }, Done::Created(id)) => {
                if let Some(page) = self.library_page() {
                    for item in page.shelves.iter_mut().flat_map(|s| &mut s.items) {
                        if item.title == title && item.target.is_none() && item.editable.is_none() {
                            item.target = Some(playlist_target(&id));
                            item.editable = Some(id.clone());
                            break;
                        }
                    }
                }
                self.account_state.created = Some((title, id));
            }
            (Undo::Added { playlist_id, .. }, Done::Added(entries)) => {
                for page in self.playlist_pages(&playlist_id) {
                    for (video_id, entry) in &entries {
                        if let Some(track) = page
                            .shelves
                            .iter_mut()
                            .flat_map(|s| &mut s.items)
                            .filter_map(|i| i.track.as_mut())
                            .find(|t| &t.video_id == video_id && t.set_video_id.is_none())
                        {
                            track.set_video_id = Some(entry.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn roll_back(&mut self, undo: Undo) {
        match undo {
            Undo::Like {
                video_id,
                set,
                before,
            } => {
                // A newer change to the same song wins.
                if self.account_state.marks.likes.get(&video_id) == Some(&set) {
                    let marks = self.marks_mut();
                    match before {
                        Some(b) => marks.likes.insert(video_id, b),
                        None => marks.likes.remove(&video_id),
                    };
                }
            }
            Undo::Saved { id, set, before } => {
                if self.account_state.marks.saved.get(&id) == Some(&set) {
                    let marks = self.marks_mut();
                    match before {
                        Some(b) => marks.saved.insert(id, b),
                        None => marks.saved.remove(&id),
                    };
                }
            }
            Undo::Subscribed { id, set, before } => {
                if self.account_state.marks.subscribed.get(&id) == Some(&set) {
                    let marks = self.marks_mut();
                    match before {
                        Some(b) => marks.subscribed.insert(id, b),
                        None => marks.subscribed.remove(&id),
                    };
                }
            }
            Undo::Created { title } => {
                if let Some(page) = self.library_page() {
                    for shelf in &mut page.shelves {
                        shelf.items.retain(|i| {
                            !(i.title == title && i.target.is_none() && i.editable.is_none())
                        });
                    }
                }
            }
            Undo::Added {
                playlist_id,
                video_ids,
                ..
            } => {
                for page in self.playlist_pages(&playlist_id) {
                    for shelf in &mut page.shelves {
                        shelf.items.retain(|i| {
                            !i.track.as_ref().is_some_and(|t| {
                                t.set_video_id.is_none() && video_ids.contains(&t.video_id)
                            })
                        });
                    }
                }
            }
            Undo::Removed {
                playlist_id,
                index,
                item,
            } => {
                for page in self.playlist_pages(&playlist_id) {
                    if let Some(shelf) = page
                        .shelves
                        .iter_mut()
                        .rev()
                        .find(|s| s.style == ShelfStyle::List)
                    {
                        let at = index.min(shelf.items.len());
                        shelf.items.insert(at, (*item).clone());
                    }
                }
            }
            Undo::Moved {
                playlist_id,
                from,
                to,
            } => {
                for page in self.playlist_pages(&playlist_id) {
                    if let Some(shelf) = page
                        .shelves
                        .iter_mut()
                        .find(|s| s.items.iter().any(|i| entry_of(i).is_some()))
                        && to < shelf.items.len()
                        && from < shelf.items.len()
                    {
                        let item = shelf.items.remove(to);
                        shelf.items.insert(from, item);
                    }
                }
            }
            Undo::Details {
                playlist_id,
                title,
                description,
            } => {
                for page in self.playlist_pages(&playlist_id) {
                    if let Some(h) = page.header.as_mut() {
                        h.title = title.clone();
                        h.description = description.clone();
                    }
                }
                if let Some(page) = self.library_page() {
                    for item in page.shelves.iter_mut().flat_map(|s| &mut s.items) {
                        if is_card_of(item, &playlist_id) {
                            item.title = title.clone();
                        }
                    }
                }
            }
            Undo::Deleted { index, item } => {
                if let Some(shelf) = self.library_page().and_then(|p| p.shelves.first_mut()) {
                    let at = index.min(shelf.items.len());
                    shelf.items.insert(at, *item);
                }
            }
        }
    }

    /// Ratings fetched from YouTube Music (the playing song's watch-next).
    pub(crate) fn account_likes(&mut self, likes: Vec<(String, LikeStatus)>) {
        for (id, status) in likes {
            self.account_state.fetched_likes.insert(id.clone(), status);
            let busy = self.account_state.subjects_in_flight();
            if self.account_state.page_wins(&id, &busy) {
                self.marks_mut().likes.insert(id, status);
            }
        }
    }

    /// Fetches again the pages a successful change affects, if they are loaded.
    pub(crate) fn account_refresh(&mut self, targets: Vec<Target>) {
        for target in targets {
            if self.pages.contains_key(&target.key()) {
                self.ensure_page(target, true);
            }
        }
    }

    /// A page arrived fresh from YouTube Music: what it says about likes,
    /// library and subscriptions replaces the app's older marks.
    pub(crate) fn account_page_fresh(&mut self, key: &str) {
        let Some(page) = self.pages.get(key).and_then(|s| s.page.as_ref()) else {
            return;
        };
        let busy = self.account_state.subjects_in_flight();
        let state = &self.account_state;
        let likes: Vec<String> = page
            .shelves
            .iter()
            .flat_map(|s| &s.items)
            .filter_map(|i| i.track.as_ref())
            .filter(|t| t.like.is_some() && state.page_wins(&t.video_id, &busy))
            .map(|t| t.video_id.clone())
            .collect();
        let header = page.header.as_ref();
        let saved = header
            .and_then(|h| h.library.as_ref())
            .map(|l| l.playlist_id.clone())
            .filter(|id| state.page_wins(id, &busy));
        let subscribed = header
            .and_then(|h| h.subscription.as_ref())
            .map(|s| s.channel_id.clone())
            .filter(|id| state.page_wins(id, &busy));
        if likes.is_empty() && saved.is_none() && subscribed.is_none() {
            return;
        }
        let marks = self.marks_mut();
        for id in likes {
            marks.likes.remove(&id);
        }
        if let Some(id) = saved {
            marks.saved.remove(&id);
        }
        if let Some(id) = subscribed {
            marks.subscribed.remove(&id);
        }
    }
}
