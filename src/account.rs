//! Changes to the signed-in account: likes and dislikes, saving albums and
//! playlists to the library, subscriptions, and the account's own playlists
//! (docs/SPEC.md § Journeys → Account; § Decisions → Write-back).
//!
//! Every change shows in the frame of the input: the app shows the new state
//! (or edits the cached pages), then the backend asks YouTube Music, one
//! write at a time in the order asked. If YouTube Music refuses, the app
//! goes back to the newest change still on its way, or to what YouTube Music
//! last said, and a plain message says so. A few seconds after a change
//! succeeds, the pages it affects are fetched again; YouTube Music takes up
//! to ~3 s to show some changes, so for a while the app's own state wins
//! over what a fetched page says.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app::{App, LibraryTab, View};
use crate::backend::Command;
use crate::model::{
    Account, Item, ItemKind, LibraryToggle, LikeStatus, Page, Run, Shelf, ShelfStyle, Subscription,
    Target, Track,
};

/// How long a change YouTube Music accepted outlives reads that don't show
/// it yet. Its reads lag its writes by 0–25 s (likes, 2026-10-01), and a read
/// that shows the change ends the wait.
const TRUST_LOCAL: Duration = Duration::from_secs(90);

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

/// The account's state as the app shows it: what YouTube Music last said,
/// or the newest change still on its way there.
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

    /// What is known about `id` for the kind of `mark`.
    fn get(&self, id: &str, mark: Mark) -> Option<Mark> {
        match mark {
            Mark::Like(_) => self.likes.get(id).copied().map(Mark::Like),
            Mark::Saved(_) => self.saved.get(id).copied().map(Mark::Saved),
            Mark::Subscribed(_) => self.subscribed.get(id).copied().map(Mark::Subscribed),
        }
    }

    fn set(&mut self, id: &str, mark: Mark) {
        match mark {
            Mark::Like(v) => self.likes.insert(id.to_owned(), v).map(drop),
            Mark::Saved(v) => self.saved.insert(id.to_owned(), v).map(drop),
            Mark::Subscribed(v) => self.subscribed.insert(id.to_owned(), v).map(drop),
        };
    }

    /// Forgets `id` for the kind of `mark`: pages speak for it again.
    fn clear(&mut self, id: &str, mark: Mark) {
        match mark {
            Mark::Like(_) => self.likes.remove(id).map(drop),
            Mark::Saved(_) => self.saved.remove(id).map(drop),
            Mark::Subscribed(_) => self.subscribed.remove(id).map(drop),
        };
    }
}

/// A rating, library state or subscription of one id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    Like(LikeStatus),
    Saved(bool),
    Subscribed(bool),
}

fn same_kind(a: Mark, b: Mark) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}

#[derive(Default)]
pub struct AccountState {
    /// What the views show; shared with them each frame (cheap to hand out).
    pub marks: Arc<Marks>,
    /// What YouTube Music last said (fresh pages, watch-next, accepted changes).
    confirmed: Marks,
    pub dialog: Option<Dialog>,
    /// The newest ratings YouTube Music returned (watch-next), as fetched.
    pub fetched_likes: HashMap<String, LikeStatus>,
    /// The last playlist created here: (title, id).
    pub created: Option<(String, String)>,
    /// The last accepted change to each id's rating, library state or
    /// subscription, until YouTube Music's reads show it.
    accepted: HashMap<String, (Instant, Mark)>,
    pending: HashMap<u64, Pending>,
    /// Changes to cached pages, applied again to every copy of an affected
    /// page that arrives while they are in flight or recent.
    overlays: Vec<Overlay>,
    /// The affected pages as YouTube Music last sent them, before the
    /// overlays: a refused change is taken back by applying the others to
    /// these again.
    bases: HashMap<String, Page>,
    next_op: u64,
    /// A move waiting for the next part of its playlist to load.
    deferred_move: Option<DeferredMove>,
}

impl AccountState {
    /// Changes still waiting for YouTube Music's answer.
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether a read of `id` as `mark` may replace what the app shows: no
    /// change to `id` is on its way, and the last accepted one is either
    /// what the read says or long enough ago.
    fn read_wins(&self, id: &str, mark: Mark) -> bool {
        !self.pending.values().any(|p| p.subject == id)
            && self.accepted.get(id).is_none_or(|(t, set)| {
                !same_kind(*set, mark) || *set == mark || t.elapsed() > TRUST_LOCAL
            })
    }

    /// Forgets page changes that YouTube Music has had time to list. Their
    /// pages as sent take them in (YouTube Music accepted them), and pages
    /// as sent that no change still in force draws on are dropped.
    fn expire_overlays(&mut self) {
        let pending = &self.pending;
        let (live, expired): (Vec<Overlay>, Vec<Overlay>) = std::mem::take(&mut self.overlays)
            .into_iter()
            .partition(|o| pending.contains_key(&o.op) || o.made.elapsed() <= TRUST_LOCAL);
        for overlay in &expired {
            for (key, base) in &mut self.bases {
                overlay.edit.apply(key, base);
            }
        }
        self.overlays = live;
        let overlays = &self.overlays;
        self.bases
            .retain(|key, base| overlays.iter().any(|o| o.edit.affects(key, base)));
    }

    /// Shows for `id` the newest change on its way, else what YouTube
    /// Music last said, else whatever the page says.
    fn settle(&mut self, id: &str, kind: Mark) {
        let newest = self
            .pending
            .iter()
            .filter(|(_, p)| p.subject == id)
            .filter_map(|(op, p)| Some((*op, p.mark.filter(|m| same_kind(*m, kind))?)))
            .max_by_key(|(op, _)| *op)
            .map(|(_, m)| m)
            .or_else(|| self.confirmed.get(id, kind));
        let marks = Arc::make_mut(&mut self.marks);
        match newest {
            Some(mark) => marks.set(id, mark),
            None => marks.clear(id, kind),
        }
    }

    /// YouTube Music says `id` is `mark` (a fresh page or watch-next).
    fn heard(&mut self, id: &str, mark: Mark) {
        if !self.read_wins(id, mark) {
            return;
        }
        if self
            .accepted
            .get(id)
            .is_some_and(|(_, set)| same_kind(*set, mark))
        {
            // Its reads have caught up (or the wait is over): they speak again.
            self.accepted.remove(id);
        }
        self.confirmed.set(id, mark);
        Arc::make_mut(&mut self.marks).set(id, mark);
    }
}

struct Pending {
    /// The id the change is about.
    subject: String,
    /// "Couldn't like “Get Lucky”."
    failed: String,
    /// What to say when YouTube Music answers that the song is already in
    /// the playlist.
    duplicate: Option<String>,
    /// The rating, library state or subscription the change sets.
    mark: Option<Mark>,
}

/// A change to cached pages that YouTube Music may not list yet. It is
/// applied when made, and again to each copy of an affected page that
/// arrives while the change is in flight or recent: a fetch that YouTube
/// Music answered from the older state must not undo it on screen.
/// Applying one twice changes nothing.
#[derive(Clone, Debug)]
enum PageEdit {
    /// A Library card for a playlist being created (`id` once known).
    Create {
        title: String,
        thumbnail: Option<String>,
        id: Option<String>,
    },
    /// Songs at the end of the playlist (with their entry ids once known).
    Add {
        playlist_id: String,
        tracks: Vec<Track>,
    },
    Remove {
        playlist_id: String,
        set_video_id: String,
    },
    /// An entry moved before another (to the end when `before` is `None`).
    Move {
        playlist_id: String,
        set_video_id: String,
        before: Option<String>,
    },
    Details {
        playlist_id: String,
        title: String,
        description: Option<String>,
    },
    Delete {
        playlist_id: String,
    },
}

struct Overlay {
    op: u64,
    made: Instant,
    edit: PageEdit,
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
        track: Some(track.clone()),
        index: None,
        stripe: None,
        editable: None,
    }
}

fn entry_of(item: &Item) -> Option<&str> {
    item.track.as_ref()?.set_video_id.as_deref()
}

fn is_card_of(item: &Item, playlist_id: &str) -> bool {
    item.editable.as_deref() == Some(playlist_id)
        || matches!(&item.target, Some(Target::Browse { id, .. }) if id.strip_prefix("VL") == Some(playlist_id))
}

/// A card added here for a playlist that YouTube Music doesn't list yet.
fn is_placeholder(item: &Item, title: &str) -> bool {
    item.title == title && item.target.is_none() && item.editable.is_none()
}

/// The playlist's own songs: its first untitled list. YouTube Music adds a
/// "Suggestions" list after it.
pub fn entries(page: &Page) -> Option<&Shelf> {
    page.shelves
        .iter()
        .find(|s| s.style == ShelfStyle::List && s.title.is_empty())
}

fn entries_mut(page: &mut Page) -> &mut Shelf {
    let at = page
        .shelves
        .iter()
        .position(|s| s.style == ShelfStyle::List && s.title.is_empty());
    let at = at.unwrap_or_else(|| {
        page.shelves.insert(
            0,
            Shelf {
                title: String::new(),
                strapline: None,
                style: ShelfStyle::List,
                items: Vec::new(),
                more: None,
                continuation: None,
            },
        );
        0
    });
    &mut page.shelves[at]
}

fn is_playlist_page(key: &str, page: &Page, playlist_id: &str) -> bool {
    key == playlist_target(playlist_id).key()
        || page
            .header
            .as_ref()
            .is_some_and(|h| h.editable.as_deref() == Some(playlist_id))
}

impl PageEdit {
    fn playlist(&self) -> Option<&str> {
        match self {
            PageEdit::Create { .. } => None,
            PageEdit::Add { playlist_id, .. }
            | PageEdit::Remove { playlist_id, .. }
            | PageEdit::Move { playlist_id, .. }
            | PageEdit::Details { playlist_id, .. }
            | PageEdit::Delete { playlist_id } => Some(playlist_id),
        }
    }

    /// Whether the page cached under `key` is one the change affects.
    fn affects(&self, key: &str, page: &Page) -> bool {
        key == library_target().key()
            || self
                .playlist()
                .is_some_and(|id| is_playlist_page(key, page, id))
    }

    /// Applies the change to the page cached under `key`, if it is one the
    /// change affects.
    fn apply(&self, key: &str, page: &mut Page) {
        if key == library_target().key() {
            self.apply_to_library(page);
        }
        if let Some(id) = self.playlist()
            && is_playlist_page(key, page, id)
        {
            self.apply_to_playlist(page);
        }
    }

    fn apply_to_library(&self, page: &mut Page) {
        match self {
            PageEdit::Create {
                title,
                thumbnail,
                id,
            } => {
                let listed = page
                    .shelves
                    .iter()
                    .flat_map(|s| &s.items)
                    .any(|i| id.as_deref().is_some_and(|id| is_card_of(i, id)));
                if listed {
                    return;
                }
                if let Some(card) = page
                    .shelves
                    .iter_mut()
                    .flat_map(|s| &mut s.items)
                    .find(|i| is_placeholder(i, title))
                {
                    if let Some(id) = id {
                        card.target = Some(playlist_target(id));
                        card.editable = Some(id.clone());
                    }
                    return;
                }
                let card = Item {
                    kind: ItemKind::Playlist,
                    title: title.clone(),
                    subtitle: vec![Run {
                        text: "Playlist".into(),
                        target: None,
                    }],
                    thumbnail: thumbnail.clone(),
                    target: id.as_deref().map(playlist_target),
                    play: None,
                    track: None,
                    index: None,
                    stripe: None,
                    editable: id.clone(),
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
            PageEdit::Details {
                playlist_id, title, ..
            } => {
                let cards = page.shelves.iter_mut().flat_map(|s| &mut s.items);
                for card in cards.filter(|c| is_card_of(c, playlist_id)) {
                    card.title = title.clone();
                }
            }
            PageEdit::Delete { playlist_id } => {
                for shelf in &mut page.shelves {
                    shelf.items.retain(|i| !is_card_of(i, playlist_id));
                }
            }
            PageEdit::Add { .. } | PageEdit::Remove { .. } | PageEdit::Move { .. } => {}
        }
    }

    fn apply_to_playlist(&self, page: &mut Page) {
        match self {
            PageEdit::Add { tracks, .. } => {
                page.message = None;
                let shelf = entries_mut(page);
                for track in tracks {
                    let row = shelf.items.iter_mut().find_map(|i| {
                        i.track.as_mut().filter(|t| {
                            t.video_id == track.video_id
                                && (t.set_video_id.is_none()
                                    || t.set_video_id == track.set_video_id)
                        })
                    });
                    match row {
                        Some(listed) => {
                            if listed.set_video_id.is_none() {
                                listed.set_video_id = track.set_video_id.clone();
                            }
                        }
                        None => shelf.items.push(row_for(track)),
                    }
                }
            }
            PageEdit::Remove { set_video_id, .. } => {
                for shelf in &mut page.shelves {
                    shelf
                        .items
                        .retain(|i| entry_of(i) != Some(set_video_id.as_str()));
                }
            }
            PageEdit::Move {
                set_video_id,
                before,
                ..
            } => {
                let shelf = entries_mut(page);
                let Some(from) = shelf
                    .items
                    .iter()
                    .position(|i| entry_of(i) == Some(set_video_id.as_str()))
                else {
                    return;
                };
                let item = shelf.items.remove(from);
                let to = match before {
                    Some(before) => shelf
                        .items
                        .iter()
                        .position(|i| entry_of(i) == Some(before.as_str())),
                    None => Some(shelf.items.len()),
                };
                // The entry it went before is gone: leave it where it was.
                shelf.items.insert(to.unwrap_or(from), item);
            }
            PageEdit::Details {
                title, description, ..
            } => {
                if let Some(h) = page.header.as_mut() {
                    h.title = title.clone();
                    h.description = description.clone();
                }
            }
            PageEdit::Create { .. } | PageEdit::Delete { .. } => {}
        }
    }
}

impl App {
    fn signed_in(&self) -> bool {
        matches!(self.account, Account::SignedIn { .. })
    }

    /// The cached pages of a playlist (it may be open under more than one key).
    fn playlist_keys(&self, playlist_id: &str) -> Vec<String> {
        self.pages
            .iter()
            .filter(|(k, s)| {
                s.page
                    .as_ref()
                    .is_some_and(|p| is_playlist_page(k, p, playlist_id))
            })
            .map(|(k, _)| k.clone())
            .collect()
    }

    fn playlist_pages(&self, playlist_id: &str) -> Vec<&Page> {
        self.pages
            .iter()
            .filter_map(|(k, s)| {
                s.page
                    .as_ref()
                    .filter(|p| is_playlist_page(k, p, playlist_id))
            })
            .collect()
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
                self.playlist_pages(playlist_id)
                    .iter()
                    .find_map(|p| Some(p.header.as_ref()?.title.clone()))
            })
            .unwrap_or_else(|| "the playlist".into())
    }

    fn send_edit(&mut self, edit: Edit, refresh: Vec<Target>, pending: Pending) -> u64 {
        self.account_state.next_op += 1;
        let op = self.account_state.next_op;
        self.account_state.pending.insert(op, pending);
        self.backend
            .send(Command::AccountEdit { op, edit, refresh });
        op
    }

    /// Shows a rating, library state or subscription at once and asks
    /// YouTube Music to make it so. `was` is what was shown: with no change
    /// to `id` on its way, that is what YouTube Music last said, and what a
    /// refusal goes back to.
    fn send_mark(
        &mut self,
        id: String,
        was: Mark,
        mark: Mark,
        edit: Edit,
        refresh: Vec<Target>,
        failed: String,
    ) {
        let state = &mut self.account_state;
        let in_flight = state.pending.values().any(|p| p.subject == id);
        if !in_flight && state.confirmed.get(&id, was).is_none() {
            state.confirmed.set(&id, was);
        }
        Arc::make_mut(&mut state.marks).set(&id, mark);
        self.send_edit(
            edit,
            refresh,
            Pending {
                subject: id,
                failed,
                duplicate: None,
                mark: Some(mark),
            },
        );
    }

    /// Changes the cached pages at once and asks YouTube Music to make the
    /// change. The affected pages as YouTube Music sent them are kept, so a
    /// refusal can be taken back.
    fn send_page_edit(
        &mut self,
        edit: Edit,
        change: PageEdit,
        refresh: Vec<Target>,
        failed: String,
        duplicate: Option<String>,
    ) {
        let state = &mut self.account_state;
        state.expire_overlays();
        for (key, page_state) in &mut self.pages {
            let Some(page) = page_state.page.as_mut() else {
                continue;
            };
            if change.affects(key, page) {
                // No change in force touches this page yet: it is as sent.
                state
                    .bases
                    .entry(key.clone())
                    .or_insert_with(|| page.clone());
                change.apply(key, page);
            }
        }
        let subject = change.playlist().map_or_else(
            || match &change {
                PageEdit::Create { title, .. } => format!("new:{title}"),
                _ => String::new(),
            },
            str::to_owned,
        );
        let op = self.send_edit(
            edit,
            refresh,
            Pending {
                subject,
                failed,
                duplicate,
                mark: None,
            },
        );
        self.account_state.overlays.push(Overlay {
            op,
            made: Instant::now(),
            edit: change,
        });
    }

    /// A copy of the page cached under `key` arrived: it becomes the base
    /// of the changes in force, which are applied to it.
    fn apply_overlays(&mut self, key: &str) {
        let state = &mut self.account_state;
        state.expire_overlays();
        let Some(page) = self.pages.get_mut(key).and_then(|s| s.page.as_mut()) else {
            return;
        };
        if !state.overlays.iter().any(|o| o.edit.affects(key, page)) {
            return;
        }
        state.bases.insert(key.to_owned(), page.clone());
        for overlay in &state.overlays {
            overlay.edit.apply(key, page);
        }
    }

    /// Redraws the affected pages from their bases with the changes in force.
    fn rebuild_pages(&mut self) {
        let state = &mut self.account_state;
        state.expire_overlays();
        for (key, base) in &state.bases {
            let Some(page) = self.pages.get_mut(key).and_then(|s| s.page.as_mut()) else {
                continue;
            };
            *page = base.clone();
            for overlay in &state.overlays {
                overlay.edit.apply(key, page);
            }
        }
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
                let was = self.account_state.marks.like(&track);
                let verb = match (status, was) {
                    (LikeStatus::Like, _) => "like",
                    (LikeStatus::Dislike, _) => "dislike",
                    (LikeStatus::Indifferent, LikeStatus::Dislike) => "remove the dislike from",
                    (LikeStatus::Indifferent, _) => "unlike",
                };
                self.send_mark(
                    track.video_id.clone(),
                    Mark::Like(was),
                    Mark::Like(status),
                    Edit::Rate {
                        video_id: track.video_id.clone(),
                        status,
                    },
                    vec![Target::browse("VLLM"), LibraryTab::Songs.target()],
                    format!("Couldn't {verb} {}.", quoted(&track.title)),
                );
            }
            AccountAction::Save {
                playlist_id,
                title,
                save,
            } => {
                let failed = if save {
                    format!("Couldn't save {} to your library.", quoted(&title))
                } else {
                    format!("Couldn't remove {} from your library.", quoted(&title))
                };
                self.send_mark(
                    playlist_id.clone(),
                    Mark::Saved(!save),
                    Mark::Saved(save),
                    Edit::Save { playlist_id, save },
                    vec![LibraryTab::Albums.target(), LibraryTab::Playlists.target()],
                    failed,
                );
            }
            AccountAction::Subscribe {
                channel_id,
                name,
                subscribe,
            } => {
                let failed = if subscribe {
                    format!("Couldn't subscribe to {}.", quoted(&name))
                } else {
                    format!("Couldn't unsubscribe from {}.", quoted(&name))
                };
                self.send_mark(
                    channel_id.clone(),
                    Mark::Subscribed(!subscribe),
                    Mark::Subscribed(subscribe),
                    Edit::Subscribe {
                        channel_id,
                        subscribe,
                    },
                    vec![
                        Target::browse("FEmusic_library_corpus_artists"),
                        LibraryTab::Artists.target(),
                    ],
                    failed,
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
                self.send_page_edit(
                    Edit::Create {
                        title: title.clone(),
                        description: description.trim().to_owned(),
                        video_ids: tracks.iter().map(|t| t.video_id.clone()).collect(),
                    },
                    PageEdit::Create {
                        title: title.clone(),
                        thumbnail: tracks.first().and_then(|t| t.thumbnail.clone()),
                        id: None,
                    },
                    vec![library_target()],
                    format!("Couldn't create {}.", quoted(&title)),
                    None,
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
                let listed = self.playlist_pages(&playlist_id).iter().any(|p| {
                    entries(p).is_some_and(|s| {
                        s.items.iter().any(|i| {
                            i.track
                                .as_ref()
                                .is_some_and(|t| tracks.iter().any(|n| n.video_id == t.video_id))
                        })
                    })
                });
                let duplicate = format!("{} is already in {}.", quoted(&song), quoted(&playlist));
                if listed {
                    self.push_error(duplicate);
                    return;
                }
                let what = if tracks.len() == 1 {
                    quoted(&song)
                } else {
                    format!("{} songs", tracks.len())
                };
                // Entry ids from another playlist mean nothing here; this
                // playlist's come with YouTube Music's answer.
                let tracks: Vec<Track> = tracks
                    .into_iter()
                    .map(|t| Track {
                        set_video_id: None,
                        ..t
                    })
                    .collect();
                self.send_page_edit(
                    Edit::Add {
                        playlist_id: playlist_id.clone(),
                        video_ids: tracks.iter().map(|t| t.video_id.clone()).collect(),
                    },
                    PageEdit::Add {
                        playlist_id: playlist_id.clone(),
                        tracks,
                    },
                    vec![playlist_target(&playlist_id), library_target()],
                    format!("Couldn't add {what} to {}.", quoted(&playlist)),
                    Some(duplicate),
                );
            }
            AccountAction::Remove {
                playlist_id,
                set_video_id,
            } => {
                let playlist = self.playlist_title(&playlist_id);
                let Some(track) = self.playlist_pages(&playlist_id).iter().find_map(|p| {
                    entries(p)?
                        .items
                        .iter()
                        .find(|i| entry_of(i) == Some(set_video_id.as_str()))?
                        .track
                        .clone()
                }) else {
                    return;
                };
                self.send_page_edit(
                    Edit::Remove {
                        playlist_id: playlist_id.clone(),
                        video_id: track.video_id.clone(),
                        set_video_id: set_video_id.clone(),
                    },
                    PageEdit::Remove {
                        playlist_id: playlist_id.clone(),
                        set_video_id,
                    },
                    vec![playlist_target(&playlist_id), library_target()],
                    format!(
                        "Couldn't remove {} from {}.",
                        quoted(&track.title),
                        quoted(&playlist)
                    ),
                    None,
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
                // The order after the move, from a page that lists both.
                let Some(plan) = self.pages.iter().find_map(|(key, s)| {
                    let page = s.page.as_ref()?;
                    if !is_playlist_page(key, page, &playlist_id) {
                        return None;
                    }
                    let shelf = entries(page)?;
                    let rows = &shelf.items;
                    let from = rows
                        .iter()
                        .position(|i| entry_of(i) == Some(set_video_id.as_str()))?;
                    let to = rows
                        .iter()
                        .position(|i| entry_of(i) == Some(onto.as_str()))?;
                    let mut order: Vec<&Item> = rows.iter().collect();
                    let item = order.remove(from);
                    order.insert(to, item);
                    Some(MovePlan {
                        key: key.clone(),
                        shelf: page
                            .shelves
                            .iter()
                            .position(|s| std::ptr::eq(s, shelf))
                            .unwrap_or(0),
                        rows: rows.len(),
                        more: shelf.continuation.clone(),
                        title: item.title.clone(),
                        successor: order.get(to + 1).map(|i| entry_of(i).map(str::to_owned)),
                    })
                }) else {
                    return;
                };
                let before = match plan.successor {
                    // Just added, not yet listed by YouTube Music.
                    Some(None) => {
                        self.push_error(format!(
                            "Couldn't move {} yet; {} is still saving. Try again in a moment.",
                            quoted(&plan.title),
                            quoted(&playlist)
                        ));
                        return;
                    }
                    Some(Some(entry)) => Some(entry),
                    // Onto the last song loaded: with more to load, the song
                    // after it isn't known yet. Moving with no successor
                    // would put it at the end of the whole playlist, so the
                    // next part is loaded first and the move made then.
                    None => match plan.more {
                        Some(token) => {
                            if let Some(state) = self.pages.get_mut(&plan.key)
                                && state.more_loading.insert(Some(plan.shelf))
                            {
                                self.backend.send(Command::More {
                                    key: plan.key.clone(),
                                    token,
                                    search: false,
                                    shelf: Some(plan.shelf),
                                });
                            }
                            let replaced = self.account_state.deferred_move.replace(DeferredMove {
                                key: plan.key,
                                rows: plan.rows,
                                title: plan.title,
                                action: AccountAction::Move {
                                    playlist_id,
                                    set_video_id: set_video_id.clone(),
                                    onto,
                                },
                            });
                            // A newer drag of the same song simply wins.
                            if let Some(old) = replaced
                                && old.moved() != Some(set_video_id.as_str())
                            {
                                self.push_error(format!(
                                    "Didn't move {}: you moved another song before the rest of the playlist loaded. Try again.",
                                    quoted(&old.title)
                                ));
                            }
                            return;
                        }
                        None => None,
                    },
                };
                let title = plan.title;
                self.send_page_edit(
                    Edit::Move {
                        playlist_id: playlist_id.clone(),
                        set_video_id: set_video_id.clone(),
                        before: before.clone(),
                    },
                    PageEdit::Move {
                        playlist_id: playlist_id.clone(),
                        set_video_id,
                        before,
                    },
                    vec![playlist_target(&playlist_id)],
                    format!("Couldn't move {} in {}.", quoted(&title), quoted(&playlist)),
                    None,
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
                let old_description = self
                    .playlist_pages(&playlist_id)
                    .iter()
                    .find_map(|p| p.header.as_ref())
                    .and_then(|h| h.description.clone())
                    .unwrap_or_default();
                let edit = Edit::Details {
                    playlist_id: playlist_id.clone(),
                    title: (title != old_title).then(|| title.clone()),
                    description: (old_description != description).then(|| description.clone()),
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
                self.send_page_edit(
                    edit,
                    PageEdit::Details {
                        playlist_id: playlist_id.clone(),
                        title,
                        description: Some(description).filter(|d| !d.is_empty()),
                    },
                    vec![playlist_target(&playlist_id), library_target()],
                    format!("Couldn't save the changes to {}.", quoted(&old_title)),
                    None,
                );
            }
            AccountAction::Delete { playlist_id } => {
                let title = self.playlist_title(&playlist_id);
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
                self.send_page_edit(
                    Edit::Delete {
                        playlist_id: playlist_id.clone(),
                    },
                    PageEdit::Delete { playlist_id },
                    vec![library_target()],
                    format!("Couldn't delete {}.", quoted(&title)),
                    None,
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
            Ok(done) => {
                let state = &mut self.account_state;
                if let Some(mark) = pending.mark {
                    state
                        .accepted
                        .insert(pending.subject.clone(), (Instant::now(), mark));
                    state.confirmed.set(&pending.subject, mark);
                    state.settle(&pending.subject, mark);
                }
                self.confirm(op, done);
            }
            Err(failure) => {
                let message = match (&failure, &pending.duplicate) {
                    (Failure::AlreadyInPlaylist, Some(duplicate)) => duplicate.clone(),
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
                    (Failure::AlreadyInPlaylist, None) => {
                        format!("{} YouTube Music didn't accept it.", pending.failed)
                    }
                };
                // Back to the newest change still on its way, or to what
                // YouTube Music last said. Other changes to the same id keep
                // their own standing against fetched pages.
                match pending.mark {
                    Some(mark) => self.account_state.settle(&pending.subject, mark),
                    None => {
                        self.account_state.overlays.retain(|o| o.op != op);
                        let targets: Vec<Target> = self
                            .account_state
                            .bases
                            .keys()
                            .filter_map(|k| Some(self.pages.get(k)?.target.clone()))
                            .collect();
                        self.rebuild_pages();
                        // YouTube Music's own copy, in case the bases lack rows loaded since.
                        for target in targets {
                            self.ensure_page(target, true);
                        }
                    }
                }
                self.push_error(message);
            }
        }
    }

    /// Records what YouTube Music returned for a change in force: a new
    /// playlist's id, new entries' ids.
    fn confirm(&mut self, op: u64, done: Done) {
        let Some(overlay) = self.account_state.overlays.iter_mut().find(|o| o.op == op) else {
            return;
        };
        match (&mut overlay.edit, done) {
            (PageEdit::Create { title, id, .. }, Done::Created(new)) => {
                *id = Some(new.clone());
                self.account_state.created = Some((title.clone(), new));
            }
            (PageEdit::Add { tracks, .. }, Done::Added(added)) => {
                for (video_id, entry) in added {
                    if let Some(t) = tracks
                        .iter_mut()
                        .find(|t| t.video_id == video_id && t.set_video_id.is_none())
                    {
                        t.set_video_id = Some(entry);
                    }
                }
            }
            _ => return,
        }
        let change = overlay.edit.clone();
        for (key, state) in &mut self.pages {
            if let Some(page) = state.page.as_mut() {
                change.apply(key, page);
            }
        }
    }

    /// Ratings fetched from YouTube Music (the playing song's watch-next).
    pub(crate) fn account_likes(&mut self, likes: Vec<(String, LikeStatus)>) {
        for (id, status) in likes {
            self.account_state.fetched_likes.insert(id.clone(), status);
            self.account_state.heard(&id, Mark::Like(status));
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

    /// A copy of a page arrived (saved or fresh). What a fresh copy says
    /// about likes, library and subscriptions becomes what the app shows
    /// (the queue's and player's copies of a song carry no rating of their
    /// own); then the page changes still in force are applied to it.
    pub(crate) fn account_page_arrived(&mut self, key: &str, cached: bool) {
        if !cached && let Some(page) = self.pages.get(key).and_then(|s| s.page.as_ref()) {
            let mut heard: Vec<(String, Mark)> = page
                .shelves
                .iter()
                .flat_map(|s| &s.items)
                .filter_map(|i| i.track.as_ref())
                .filter_map(|t| Some((t.video_id.clone(), Mark::Like(t.like?))))
                .collect();
            if let Some(h) = page.header.as_ref() {
                if let Some(l) = &h.library {
                    heard.push((l.playlist_id.clone(), Mark::Saved(l.saved)));
                }
                if let Some(s) = &h.subscription {
                    heard.push((s.channel_id.clone(), Mark::Subscribed(s.subscribed)));
                }
            }
            for (id, mark) in heard {
                self.account_state.heard(&id, mark);
            }
        }
        self.apply_overlays(key);
        // A move waiting on this playlist's next part: the page was replaced,
        // so the part it asked for won't arrive. Plan it again on this copy
        // (it asks for the next part again if it still needs it).
        if self
            .account_state
            .deferred_move
            .as_ref()
            .is_some_and(|d| d.key == key)
            && let Some(deferred) = self.account_state.deferred_move.take()
        {
            let listed = |entry: &str| {
                self.pages
                    .get(key)
                    .and_then(|s| s.page.as_ref())
                    .and_then(entries)
                    .is_some_and(|s| s.items.iter().any(|i| entry_of(i) == Some(entry)))
            };
            match &deferred.action {
                AccountAction::Move {
                    set_video_id, onto, ..
                } if listed(set_video_id) && listed(onto) => self.account_action(deferred.action),
                _ => self.push_error(format!(
                    "Couldn't move {}: the playlist changed while it loaded. Try again.",
                    quoted(&deferred.title)
                )),
            }
        }
    }

    /// More rows of a page arrived: a move waiting for the rows after the
    /// one it was dropped on is made now.
    pub(crate) fn account_more_arrived(&mut self, key: &str) {
        if self
            .account_state
            .deferred_move
            .as_ref()
            .is_none_or(|d| d.key != key)
        {
            return;
        }
        let Some(deferred) = self.account_state.deferred_move.take() else {
            return;
        };
        let rows = self
            .pages
            .get(key)
            .and_then(|s| s.page.as_ref())
            .and_then(entries)
            .map_or(0, |s| s.items.len());
        if rows > deferred.rows {
            self.account_action(deferred.action);
        } else {
            self.push_error(format!(
                "Couldn't move {}: the rest of the playlist didn't load. Try again.",
                quoted(&deferred.title)
            ));
        }
    }
}

/// Where a dragged song would land, worked out from a loaded page.
struct MovePlan {
    key: String,
    /// The playlist's own list on that page.
    shelf: usize,
    rows: usize,
    more: Option<String>,
    title: String,
    /// The entry after its new place: `None` past the last row loaded,
    /// `Some(None)` when that row isn't saved yet.
    successor: Option<Option<String>>,
}

/// A move waiting for the next part of the playlist to load.
struct DeferredMove {
    key: String,
    /// Rows loaded when it was asked for.
    rows: usize,
    title: String,
    action: AccountAction,
}

impl DeferredMove {
    /// The entry being moved.
    fn moved(&self) -> Option<&str> {
        match &self.action {
            AccountAction::Move { set_video_id, .. } => Some(set_video_id),
            _ => None,
        }
    }
}
