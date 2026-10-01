//! Application state. Views (in `ui`) read it and push [`Action`]s; the
//! app applies them after the frame and turns them into backend commands.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::backend::{Backend, Command, Event};
use crate::desktop::{Flags, Request};
use crate::model::{Account, Lyrics, Page, Playback, Target, Track};
use crate::parse::More;
use crate::paths::Paths;
use crate::theme::Palette;

/// Which window the app shows: the full window or the mini player. They
/// are separate native windows; switching closes one and opens the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WindowKind {
    #[default]
    Main,
    Mini,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryTab {
    Playlists,
    Songs,
    Albums,
    Artists,
    /// What the account played, by day.
    History,
}

impl LibraryTab {
    pub const ALL: [LibraryTab; 5] = [
        LibraryTab::Playlists,
        LibraryTab::Songs,
        LibraryTab::Albums,
        LibraryTab::Artists,
        LibraryTab::History,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LibraryTab::Playlists => "Playlists",
            LibraryTab::Songs => "Songs",
            LibraryTab::Albums => "Albums",
            LibraryTab::Artists => "Artists",
            LibraryTab::History => "History",
        }
    }

    pub fn target(self) -> Target {
        Target::browse(match self {
            LibraryTab::Playlists => "FEmusic_liked_playlists",
            LibraryTab::Songs => "FEmusic_liked_videos",
            LibraryTab::Albums => "FEmusic_liked_albums",
            LibraryTab::Artists => "FEmusic_library_corpus_track_artists",
            LibraryTab::History => "FEmusic_history",
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Home,
    Explore,
    Library(LibraryTab),
    /// Album, artist, playlist, mood, chart, search results…
    Page(Target),
}

impl View {
    pub fn target(&self) -> Target {
        match self {
            View::Home => Target::browse("FEmusic_home"),
            View::Explore => Target::browse("FEmusic_explore"),
            View::Library(tab) => tab.target(),
            View::Page(target) => target.clone(),
        }
    }

    /// Where a target leads: a page to open, or `None` for playback targets.
    pub fn for_target(target: &Target) -> Option<View> {
        match target {
            Target::Browse { id, params: None } if id == "FEmusic_home" => Some(View::Home),
            Target::Browse { id, params: None } if id == "FEmusic_explore" => Some(View::Explore),
            Target::Watch { .. } => None,
            other => Some(View::Page(other.clone())),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NowPlayingTab {
    #[default]
    UpNext,
    Lyrics,
    Related,
}

pub struct PageState {
    pub target: Target,
    pub page: Option<Page>,
    pub loading: bool,
    /// The page shown is the saved copy and a refresh is under way or failed.
    pub cached: bool,
    pub error: Option<String>,
    /// Continuations in flight: `None` for the page, `Some(i)` for shelf i.
    pub more_loading: HashSet<Option<usize>>,
    pub fetched: Option<Instant>,
    /// The newest request for this page; older answers are ignored.
    seq: u64,
    /// A chip's in-place reload in flight: its continuation and chip index.
    reload: Option<(String, usize)>,
}

pub enum Action {
    Open(View),
    Back,
    /// Activate a target: open its page or start playing it.
    Activate(Target),
    Play {
        tracks: Vec<Track>,
        start: usize,
    },
    Command(Command),
    More {
        key: String,
        token: String,
        search: bool,
        shelf: Option<usize>,
    },
    Search(String),
    NowPlaying(bool),
    NowPlayingTab(NowPlayingTab),
    Retry(String),
    /// Fetch a page without opening it (Now Playing's Related tab).
    Load(Target),
    /// Lyrics for the song with this video id.
    Lyrics(String),
    /// Forget a failed lyrics fetch (by video id) so it is asked for again.
    RetryLyrics(String),
    /// Drop one recent search.
    ForgetSearch(String),
    ClearSearches,
    /// A chip that swaps page `key`'s shelves in place (`Chip::reload`).
    ReloadChip {
        key: String,
        chip: usize,
        token: String,
    },
    DismissError(usize),
    Copy(String),
    /// The pointer rests on a song: resolve it ahead of a likely click.
    Prepare(String),
    /// Quit for real (Ctrl+Q): playback stops.
    Quit,
    /// Switch to the mini player (`true`) or back to the full window.
    MiniPlayer(bool),
    /// Settings: song-change notifications on or off.
    Notifications(bool),
    /// Like the playing song, or remove its like.
    ToggleLikeCurrent,
    /// Likes, library, subscriptions and playlists (see `crate::account`).
    Account(crate::account::AccountAction),
    /// Show or hide the equalizer.
    ShowEqualizer(bool),
    /// Open (`true`) or close Stage, the full-window cover and lyrics.
    Stage(bool),
    /// Inside Stage: the window to full screen and back (F11).
    StageFullscreen,
    /// Seek to the start of the playing song's most replayed part.
    JumpToPeak,
    /// Settings: covers outside Now Playing and Stage in theme colours.
    PaintCovers(bool),
    /// A song is held under the pointer this frame (Alt or the middle
    /// button): audition it. Not pushing it ends the audition.
    Audition(Track),
}

pub struct App {
    pub backend: Backend,
    pub palette: Palette,
    applied: Option<Palette>,
    themes: fastframe_theme::Catalog<Palette>,
    transition: fastframe_theme::Transition,
    paths: Paths,
    /// Detected once; each new window gets a copy.
    fonts: egui::FontDefinitions,
    /// Repaints whichever window is open, and nothing while none is.
    waker: fastframe_shell::Waker,
    /// What MPRIS and the command line ask of the interface.
    requests: std::sync::mpsc::Receiver<Request>,
    /// Feeds `requests`: links in files dropped on the window, read off the UI thread.
    request_tx: std::sync::mpsc::Sender<Request>,
    /// Shared with MPRIS and notifications.
    pub desktop: Arc<Flags>,
    reload_themes: bool,
    /// The kind of window open, or to open next.
    pub window: WindowKind,
    /// No window is open: it was closed while music played, and the app runs
    /// on in the background until it is shown again or quits.
    pub hidden: bool,
    /// The app ends when the window closes (Ctrl+Q, `ytfast quit`, MPRIS Quit).
    pub(crate) quit_requested: bool,
    /// The window closes to reopen as the other kind.
    switch_window: bool,
    /// Show was asked for while no window was open.
    wants_show: bool,

    pub account: Account,
    /// Browser profiles signed in to YouTube, and the one in use.
    pub profiles: Vec<crate::auth::Profile>,
    pub profile: Option<String>,
    pub view: View,
    pub history: Vec<View>,
    pub pages: HashMap<String, PageState>,
    pub search: String,
    pub suggestions: Vec<String>,
    suggested_for: String,
    pub queue: Vec<Track>,
    pub playback: Playback,
    pub now_playing: bool,
    pub now_playing_tab: NowPlayingTab,
    pub lyrics: HashMap<String, Result<Option<Lyrics>, String>>,
    lyrics_requested: HashSet<String>,
    /// When `playback` last arrived, to move the position on between updates.
    playback_at: Instant,
    /// Recent searches, newest first.
    pub recent_searches: Vec<String>,
    /// The playing song's cover colours (for Now Playing and Stage), `None`
    /// until worked out or when the cover has none.
    pub cover_colors: Option<crate::colors::CoverColors>,
    cover_extractor: crate::colors::Extractor,
    /// Now Playing's move from the previous cover's colours.
    pub cover_fade: crate::colors::Fade,
    /// The cover `cover_colors` were taken from.
    pub cover_url: Option<String>,
    last_prepared: Option<String>,
    page_seq: u64,
    pub errors: Vec<String>,
    pub scroll_to_top: bool,
    pub started: Instant,
    /// How long the first frame took after launch.
    pub first_frame: Option<Duration>,
    /// Most-replayed heat by video id: `None` when the song has none.
    pub heat: HashMap<String, Option<Arc<crate::heat::Heat>>>,
    heat_requested: HashSet<String>,
    /// Covers outside Now Playing and Stage are drawn in the theme's colours.
    pub paint_covers: bool,
    /// Stage: the window filled with the cover and lyrics.
    pub stage: crate::ui::stage::Stage,
    #[cfg(feature = "e2e")]
    pub e2e: Option<crate::e2e::Driver>,
    /// The desktop's palette has been applied once; later changes animate.
    themed: bool,
    /// Likes, library and subscription marks, changes in flight, dialogs.
    pub account_state: crate::account::AccountState,
    /// The equalizer window is open.
    pub equalizer_open: bool,
    /// The songs last prepared for being on screen.
    on_screen: Vec<String>,
    /// The song held for an audition this frame, and the one auditioned.
    audition_held: Option<Track>,
    auditioning: Option<String>,
    /// Alt went down with another key (Alt+←): a shortcut, not an
    /// audition, until Alt is up again.
    audition_blocked: bool,
}

impl App {
    /// The app's state, which outlives its windows; [`App::attach`] sets up
    /// each window.
    pub fn new(
        backend: Backend,
        paths: Paths,
        desktop: Arc<Flags>,
        (request_tx, requests): (
            std::sync::mpsc::Sender<Request>,
            std::sync::mpsc::Receiver<Request>,
        ),
        waker: fastframe_shell::Waker,
        started: Instant,
    ) -> Self {
        let mut fonts = fastframe_fonts::FontSetup::default().definitions();
        fastframe_text::detect().apply_to(&mut fonts);
        let palette = Palette::default();

        let mut themes = fastframe_theme::Catalog::default();
        themes.enable_desktop_themes(fastframe_theme::DesktopThemes {
            slug: "ytfast",
            omarchy_template: fastframe_theme::omarchy::BASE_TEMPLATE,
            omarchy_previous_templates: &[],
            presets: false,
        });
        let paint_covers = crate::settings::Settings::load(&paths).paint_covers;
        let mut app = Self {
            backend,
            palette,
            applied: None,
            themes,
            transition: fastframe_theme::Transition::new(fastframe_theme::Reveal::Band),
            paths,
            fonts,
            waker,
            requests,
            request_tx,
            desktop,
            reload_themes: false,
            window: WindowKind::Main,
            hidden: false,
            quit_requested: false,
            switch_window: false,
            wants_show: false,
            account: Account::Checking,
            profiles: Vec::new(),
            profile: None,
            view: View::Home,
            history: Vec::new(),
            pages: HashMap::new(),
            search: String::new(),
            suggestions: Vec::new(),
            suggested_for: String::new(),
            queue: Vec::new(),
            playback: Playback::default(),
            now_playing: false,
            now_playing_tab: NowPlayingTab::default(),
            lyrics: HashMap::new(),
            lyrics_requested: HashSet::new(),
            playback_at: Instant::now(),
            recent_searches: Vec::new(),
            cover_colors: None,
            cover_extractor: crate::colors::Extractor::default(),
            cover_fade: crate::colors::Fade::default(),
            cover_url: None,
            last_prepared: None,
            page_seq: 0,
            errors: Vec::new(),
            scroll_to_top: false,
            started,
            first_frame: None,
            account_state: Default::default(),
            #[cfg(feature = "e2e")]
            e2e: crate::e2e::Driver::from_env(),
            themed: false,
            equalizer_open: false,
            on_screen: Vec::new(),
            heat: HashMap::new(),
            heat_requested: HashSet::new(),
            paint_covers,
            stage: crate::ui::stage::Stage::default(),
            audition_held: None,
            auditioning: None,
            audition_blocked: false,
        };
        app.start_themes();
        app.ensure_page(View::Home.target(), false);
        app.ensure_page(LibraryTab::Playlists.target(), false);
        app.backend.send(Command::LoadSearches);
        app
    }

    /// Sets up a new window: fonts, image loaders, icons, the palette.
    pub fn attach(&mut self, ctx: &egui::Context) {
        ctx.set_fonts(self.fonts.clone());
        egui_extras::install_image_loaders(ctx);
        fastframe_icons::install::<crate::icons::Icon>(ctx);
        ctx.add_bytes_loader(Arc::new(crate::covers::CoverLoader::new(
            self.backend.runtime.clone(),
            self.backend.http.clone(),
            self.paths.clone(),
        )));
        ctx.add_image_loader(Arc::new(crate::derived::Loader::new(
            self.backend.runtime.clone(),
        )));
        ctx.options_mut(|o| o.reduce_texture_memory = true);
        crate::theme::apply(ctx, &self.palette);
        self.applied = Some(self.palette.clone());
        self.transition = fastframe_theme::Transition::new(fastframe_theme::Reveal::Band);
        self.hidden = false;
        self.wants_show = false;
        self.switch_window = false;
    }

    fn start_themes(&mut self) {
        let waker = self.waker.clone();
        let waker = fastframe_theme::Waker::new(move || waker.wake());
        self.themes.start(
            self.paths.config.join("themes"),
            Some(fastframe_theme::omarchy::FILENAME.into()),
            &waker,
        );
    }

    pub fn page_state(&self, target: &Target) -> Option<&PageState> {
        self.pages.get(&target.key())
    }

    /// Asks for a page unless a fresh copy is loaded or loading.
    pub fn ensure_page(&mut self, target: Target, force: bool) {
        let key = target.key();
        let stale = |s: &PageState| {
            s.fetched
                .is_none_or(|t| t.elapsed() > Duration::from_secs(300))
                || s.error.is_some()
        };
        let needed = match self.pages.get(&key) {
            None => true,
            // A forced refresh wins over a fetch already under way (sign-in).
            Some(s) => force || (!s.loading && stale(s)),
        };
        if !needed {
            return;
        }
        self.page_seq += 1;
        let seq = self.page_seq;
        let state = self.pages.entry(key).or_insert_with(|| PageState {
            target: target.clone(),
            page: None,
            loading: false,
            cached: false,
            error: None,
            more_loading: HashSet::new(),
            fetched: None,
            seq,
            reload: None,
        });
        state.loading = true;
        state.error = None;
        state.reload = None;
        state.seq = seq;
        self.backend.send(Command::Page { target, seq });
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Account(account) => {
                let was_signed_in = matches!(self.account, Account::SignedIn { .. });
                let signed_in = matches!(account, Account::SignedIn { .. });
                self.account = account;
                if signed_in && !was_signed_in {
                    // Pages fetched before the session was confirmed may be public copies.
                    self.ensure_page(self.view.target(), true);
                    self.ensure_page(LibraryTab::Playlists.target(), true);
                    if self.view != View::Home {
                        self.ensure_page(View::Home.target(), true);
                    }
                }
            }
            Event::Page {
                key,
                seq,
                result,
                cached,
            } => {
                let Some(state) = self.pages.get_mut(&key) else {
                    return;
                };
                if seq != state.seq {
                    return;
                }
                match result {
                    Ok(page) => {
                        // A late cached copy never replaces fresh content.
                        if cached && state.page.is_some() && !state.cached {
                            return;
                        }
                        state.page = Some(*page);
                        state.cached = cached;
                        if !cached {
                            state.loading = false;
                            state.error = None;
                            state.fetched = Some(Instant::now());
                            state.more_loading.clear();
                        }
                    }
                    Err(error) => {
                        state.loading = false;
                        state.error = Some(error);
                    }
                }
                if self.pages.get(&key).is_some_and(|s| s.error.is_none()) {
                    self.account_page_arrived(&key, cached);
                }
            }
            Event::More {
                key,
                shelf,
                token,
                result,
            } => {
                let Some(state) = self.pages.get_mut(&key) else {
                    return;
                };
                let Some(page) = state.page.as_mut() else {
                    return;
                };
                // A chip's in-place reload (an artist's Albums / Singles & EPs).
                if shelf.is_none() && state.reload.as_ref().is_some_and(|(t, _)| *t == token) {
                    let chip = state.reload.take().map_or(0, |(_, chip)| chip);
                    state.more_loading.remove(&None);
                    match result {
                        Ok(More::Shelves { shelves, next }) => {
                            page.shelves = shelves;
                            page.continuation = next;
                            for (i, c) in page.chips.iter_mut().enumerate() {
                                c.selected = i == chip;
                            }
                        }
                        Ok(More::Items { .. }) => {}
                        Err(error) => self.push_error(format!("Couldn't load that: {error}")),
                    }
                    return;
                }
                // Only the answer to the token still on the page applies; a
                // refreshed page has its own.
                let slot = match shelf {
                    None => &mut page.continuation,
                    Some(i) => match page.shelves.get_mut(i) {
                        Some(s) => &mut s.continuation,
                        None => return,
                    },
                };
                if slot.as_deref() != Some(token.as_str()) {
                    return;
                }
                state.more_loading.remove(&shelf);
                match result {
                    Ok(More::Shelves { shelves, next }) => {
                        page.shelves.extend(shelves);
                        page.continuation = next;
                    }
                    Ok(More::Items { items, next }) => {
                        match shelf.and_then(|i| page.shelves.get_mut(i)) {
                            Some(s) => {
                                s.items.extend(items);
                                s.continuation = next;
                            }
                            None => page.continuation = None,
                        }
                    }
                    Err(error) => {
                        // Stop asking for this part; a page refresh starts over.
                        *slot = None;
                        self.push_error(format!("Couldn't load more: {error}"));
                    }
                }
            }
            Event::Suggestions { input, items } => {
                if input == self.search {
                    self.suggestions = items;
                }
            }
            Event::Lyrics { id, result } => {
                self.lyrics.insert(id, result);
            }
            Event::Queue(queue) => self.queue = queue,
            Event::Playback(playback) => {
                self.playback = playback;
                self.playback_at = Instant::now();
            }
            Event::Searches(saved) => {
                // Searches made before the saved list arrived stay first.
                for query in saved {
                    if !self
                        .recent_searches
                        .iter()
                        .any(|q| q.eq_ignore_ascii_case(&query))
                    {
                        self.recent_searches.push(query);
                    }
                }
                self.recent_searches.truncate(crate::searches::KEEP);
            }
            Event::Error(error) => self.push_error(error),
            Event::Profiles { list, current } => {
                self.profiles = list;
                self.profile = current;
            }
            Event::AccountEdited { op, result } => self.account_edited(op, result),
            Event::Likes(likes) => self.account_likes(likes),
            Event::AccountRefresh(targets) => self.account_refresh(targets),
            Event::Heat { id, heat } => {
                self.heat.insert(id, heat.map(Arc::new));
            }
        }
    }

    pub fn push_error(&mut self, error: String) {
        self.errors.retain(|e| *e != error);
        self.errors.push(error);
        if self.errors.len() > 3 {
            self.errors.remove(0);
        }
    }

    pub(crate) fn open(&mut self, view: View) {
        if view != self.view {
            let previous = std::mem::replace(&mut self.view, view);
            self.history.push(previous);
            if self.history.len() > 50 {
                self.history.remove(0);
            }
        }
        self.now_playing = false;
        self.scroll_to_top = true;
        self.ensure_page(self.view.target(), false);
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Open(view) => self.open(view),
            Action::Back => {
                if let Some(view) = self.history.pop() {
                    // The header's cover flies back to the card it came from.
                    crate::ui::motion::launch_from_origin(ctx, "header", None);
                    self.view = view;
                    self.now_playing = false;
                    self.ensure_page(self.view.target(), false);
                }
            }
            Action::Activate(target) => match View::for_target(&target) {
                Some(view) => self.open(view),
                None => self.backend.send(Command::PlayTarget(target)),
            },
            Action::Play { tracks, start } => {
                self.backend.send(Command::PlayTracks { tracks, start })
            }
            Action::Command(command) => {
                self.optimistic(&command);
                self.backend.send(command)
            }
            Action::More {
                key,
                token,
                search,
                shelf,
            } => {
                if let Some(state) = self.pages.get_mut(&key)
                    && state.more_loading.insert(shelf)
                {
                    self.backend.send(Command::More {
                        key,
                        token,
                        search,
                        shelf,
                    });
                }
            }
            Action::Search(query) => {
                let query = query.trim().to_owned();
                if crate::links::target_from_link(&query).is_some() {
                    // A pasted link opens what it links to.
                    self.search.clear();
                    self.suggestions.clear();
                    ctx.memory_mut(|m| m.stop_text_input());
                    self.open_link(ctx, &query);
                } else if !query.is_empty() {
                    self.search = query.clone();
                    self.suggestions.clear();
                    crate::searches::remember(&mut self.recent_searches, &query);
                    self.save_searches();
                    self.open(View::Page(Target::Search {
                        query,
                        params: None,
                    }));
                }
            }
            Action::NowPlaying(open) => {
                let open = open && !self.queue.is_empty();
                match (self.now_playing, open) {
                    (false, true) => crate::ui::motion::launch_from_origin(
                        ctx,
                        "player",
                        Some(crate::ui::motion::now_playing_site()),
                    ),
                    (true, false) => crate::ui::motion::launch_from_origin(
                        ctx,
                        "now-playing",
                        Some(crate::ui::motion::player_site()),
                    ),
                    _ => {}
                }
                self.now_playing = open;
            }
            Action::NowPlayingTab(tab) => self.now_playing_tab = tab,
            Action::Load(target) => self.ensure_page(target, false),
            Action::Retry(key) => {
                if let Some(target) = self.pages.get(&key).map(|s| s.target.clone()) {
                    self.ensure_page(target, true);
                }
            }
            Action::RetryLyrics(id) => {
                self.lyrics.remove(&id);
                self.lyrics_requested.remove(&id);
            }
            Action::Lyrics(id) => self.request_lyrics(&id),
            Action::ForgetSearch(query) => {
                self.recent_searches.retain(|q| *q != query);
                self.save_searches();
            }
            Action::ClearSearches => {
                self.recent_searches.clear();
                self.save_searches();
            }
            Action::ReloadChip { key, chip, token } => {
                if let Some(state) = self.pages.get_mut(&key)
                    && state.page.is_some()
                {
                    state.reload = Some((token.clone(), chip));
                    state.more_loading.insert(None);
                    self.backend.send(Command::More {
                        key,
                        token,
                        search: false,
                        shelf: None,
                    });
                }
            }
            Action::DismissError(i) => {
                if i < self.errors.len() {
                    self.errors.remove(i);
                }
            }
            Action::Copy(text) => ctx.copy_text(text),
            Action::Prepare(video_id) => {
                if self.last_prepared.as_ref() != Some(&video_id) {
                    self.last_prepared = Some(video_id.clone());
                    self.backend.send(Command::Prepare(video_id));
                }
            }
            Action::Quit => self.quit(ctx),
            Action::MiniPlayer(on) => {
                if !on || !self.queue.is_empty() {
                    self.switch(
                        ctx,
                        if on {
                            WindowKind::Mini
                        } else {
                            WindowKind::Main
                        },
                    );
                }
            }
            Action::Notifications(on) => {
                self.desktop.notifications.store(on, Ordering::Relaxed);
                self.backend.send(Command::Notifications(on));
            }
            Action::ToggleLikeCurrent => self.toggle_like_current(),
            Action::Account(action) => self.account_action(action),
            Action::ShowEqualizer(open) => self.equalizer_open = open,
            Action::Stage(open) => self.set_stage(ctx, open),
            Action::StageFullscreen => {
                if self.stage.open {
                    let on = !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
                    self.stage.fullscreen = on;
                }
            }
            Action::JumpToPeak => {
                if let Some(peak) = self.current_heat().and_then(|h| h.peak) {
                    self.backend.send(Command::Seek(peak.start));
                }
            }
            Action::PaintCovers(on) => {
                self.paint_covers = on;
                self.backend.send(Command::PaintCovers(on));
            }
            Action::Audition(track) => self.audition_held = Some(track),
        }
    }

    /// After a frame's actions: what is held under the pointer becomes the
    /// audition, and nothing held ends it. Alt pressed with another key is a
    /// shortcut (Alt+← Back), so it auditions nothing until Alt is let go.
    fn audition_frame(&mut self, ctx: &egui::Context) {
        let (alt, key, middle) = ctx.input(|i| {
            let key = i
                .events
                .iter()
                .any(|e| matches!(e, egui::Event::Key { pressed: true, .. }));
            (i.modifiers.alt, key, i.pointer.middle_down())
        });
        if !alt {
            self.audition_blocked = false;
        } else if key {
            self.audition_blocked = true;
        }
        let held = self
            .audition_held
            .take()
            .filter(|_| middle || !self.audition_blocked);
        match held {
            Some(track) if self.auditioning.as_deref() != Some(track.video_id.as_str()) => {
                self.auditioning = Some(track.video_id.clone());
                // The best part: the most replayed point when YouTube's heat
                // for the song is known, else a guess.
                let start = self
                    .heat
                    .get(&track.video_id)
                    .and_then(|h| h.as_ref())
                    .and_then(|h| h.peak)
                    .map(|peak| peak.start)
                    .or_else(|| crate::ui::audition::best_part(&track));
                self.backend.send(Command::Audition { track, start });
            }
            Some(_) => {}
            None => self.end_audition(),
        }
    }

    fn end_audition(&mut self) {
        if self.auditioning.take().is_some() {
            self.backend.send(Command::EndAudition);
        }
    }

    /// Opens or closes Stage: the cover flies between Stage and the place it
    /// shows below it (Now Playing, or the player bar).
    fn set_stage(&mut self, ctx: &egui::Context, open: bool) {
        use crate::ui::motion;
        let open = open && !self.queue.is_empty();
        if open == self.stage.open {
            return;
        }
        let (site, place) = if self.now_playing {
            ("now-playing", motion::now_playing_site())
        } else {
            ("player", motion::player_site())
        };
        if open {
            motion::launch_from_origin(ctx, site, Some(motion::stage_site()));
        } else {
            motion::launch_from_origin(ctx, "stage", Some(place));
            if std::mem::take(&mut self.stage.fullscreen) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
        }
        self.stage.open = open;
        if open {
            self.stage.opened += 1;
        }
        self.stage.clear_frames();
    }

    /// The playing song's most-replayed heat, once known (and if it has any).
    pub fn current_heat(&self) -> Option<&crate::heat::Heat> {
        self.heat.get(&self.current_track()?.video_id)?.as_deref()
    }

    /// Asks once for the playing song's most-replayed heat.
    fn heat_frame(&mut self) {
        let Some(id) = self.current_track().map(|t| t.video_id.clone()) else {
            return;
        };
        if self.heat_requested.insert(id.clone()) {
            self.backend.send(Command::Heat(id));
        }
    }

    /// Opens a YouTube Music or YouTube link: a song starts playing, a page
    /// opens (and the window comes back to show it).
    pub(crate) fn open_link(&mut self, ctx: &egui::Context, link: &str) {
        let Some(target) = crate::links::target_from_link(link) else {
            self.push_error("That isn't a YouTube Music or YouTube link.".into());
            return;
        };
        log::info!("opening a link: {}", target.key());
        match View::for_target(&target) {
            Some(view) => {
                self.open(view);
                if self.window == WindowKind::Mini {
                    self.switch(ctx, WindowKind::Main);
                }
                self.show(ctx);
            }
            None => self.backend.send(Command::PlayTarget(target)),
        }
    }

    /// Brings the window back: reopens it if it was closed, else raises it.
    fn show(&mut self, ctx: &egui::Context) {
        if self.hidden {
            self.wants_show = true;
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    /// Quits for real: the window closes and the app ends, stopping playback.
    pub(crate) fn quit(&mut self, ctx: &egui::Context) {
        self.quit_requested = true;
        if !self.hidden {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Closes this window to open the other kind in its place.
    fn switch(&mut self, ctx: &egui::Context, kind: WindowKind) {
        if kind == self.window {
            return;
        }
        self.window = kind;
        if self.hidden {
            self.wants_show = true;
        } else {
            self.switch_window = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Backend events and outside requests: every frame, and every tick of
    /// the background loop while no window is open.
    fn background(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.backend.events.try_recv() {
            self.handle(event);
        }
        while let Ok(request) = self.requests.try_recv() {
            match request {
                Request::Show => self.show(ctx),
                Request::Quit => self.quit(ctx),
                Request::Open(link) => self.open_link(ctx, &link),
                Request::Like => self.toggle_like_current(),
                Request::ReloadThemes => self.reload_themes = true,
            }
        }
    }

    /// Links dropped on the window. winit delivers dropped files (X11; the
    /// pinned Wayland backend delivers no drops at all), so a `.url` file or
    /// a text file holding a link opens it; the file is read off this thread.
    fn dropped(&self, ctx: &egui::Context) {
        let files = ctx.input(|i| i.raw.dropped_files.clone());
        for file in files {
            let path = file.path().to_path_buf();
            let tx = self.request_tx.clone();
            let waker = self.waker.clone();
            self.backend.runtime.spawn(async move {
                use tokio::io::AsyncReadExt;
                let Ok(file) = tokio::fs::File::open(&path).await else {
                    return;
                };
                let mut text = Vec::new();
                if file.take(64 * 1024).read_to_end(&mut text).await.is_ok()
                    && let Some(link) = crate::links::find_link(&String::from_utf8_lossy(&text))
                {
                    let _ = tx.send(Request::Open(link.to_owned()));
                    waker.wake();
                }
            });
        }
    }

    fn save_searches(&self) {
        self.backend
            .send(Command::SaveSearches(self.recent_searches.clone()));
    }

    /// The song playing (or loading) now.
    pub fn current_track(&self) -> Option<&Track> {
        self.playback.index.and_then(|i| self.queue.get(i))
    }

    /// The playing song's lyrics: `None` until asked for and answered.
    pub fn current_lyrics(&self) -> Option<&Result<Option<Lyrics>, String>> {
        self.lyrics.get(&self.current_track()?.video_id)
    }

    /// The playback position now, moved on from the last backend update
    /// (which comes at most four times a second) while playing.
    pub fn position_now(&self) -> f64 {
        let p = &self.playback;
        if !p.playing || p.loading {
            return p.position;
        }
        // Past two seconds without an update something is stuck: don't run on.
        let since = self.playback_at.elapsed().as_secs_f64().min(2.0);
        let now = p.position + since;
        if p.duration > 0.0 {
            now.min(p.duration)
        } else {
            now
        }
    }

    /// Asks once for the lyrics of the queued song `video_id`.
    fn request_lyrics(&mut self, video_id: &str) {
        if self.lyrics_requested.contains(video_id) {
            return;
        }
        let Some(track) = self.queue.iter().find(|t| t.video_id == video_id).cloned() else {
            return;
        };
        let current = self.current_track().map(|t| t.video_id.as_str()) == Some(video_id);
        let browse_id = current.then(|| self.playback.lyrics.clone()).flatten();
        let duration = track
            .duration
            .map(f64::from)
            .or(current.then_some(self.playback.duration))
            .unwrap_or(0.0);
        self.lyrics_requested.insert(video_id.to_owned());
        self.backend.send(Command::Lyrics {
            track,
            browse_id,
            duration,
        });
    }

    /// Lyrics for the playing song are asked for ahead of the Lyrics tab:
    /// once YouTube Music has named its lyrics page, or a moment into the
    /// song if it hasn't (then only LRCLIB may have them).
    fn lyrics_frame(&mut self) {
        let Some(track) = self.playback.index.and_then(|i| self.queue.get(i)) else {
            return;
        };
        if !self.lyrics_requested.contains(&track.video_id)
            && (self.playback.lyrics.is_some() || self.playback.position >= 3.0)
        {
            let id = track.video_id.clone();
            self.request_lyrics(&id);
        }
    }

    /// Works out the playing cover's colours off this thread and starts
    /// Now Playing's move to them when they change.
    fn cover_frame(&mut self, ctx: &egui::Context) {
        let Some(url) = self
            .playback
            .index
            .and_then(|i| self.queue.get(i))
            .and_then(|t| t.thumbnail.as_deref())
        else {
            return;
        };
        let Some(colors) = self.cover_extractor.get(ctx, &self.backend.runtime, url) else {
            return;
        };
        if colors != self.cover_colors {
            self.cover_fade
                .start(self.cover_colors.as_ref(), &self.palette);
            self.cover_colors = colors;
        }
        if self.cover_url.as_deref() != Some(url) {
            self.cover_url = Some(url.to_owned());
        }
    }

    /// Playback edits show in the same frame; the backend's state follows.
    fn optimistic(&mut self, command: &Command) {
        let current = self.playback.index;
        match command {
            &Command::MoveInQueue { from, to } if from < self.queue.len() => {
                let track = self.queue.remove(from);
                let to = to.min(self.queue.len());
                self.queue.insert(to, track);
                self.playback.index = current.map(|c| {
                    if c == from {
                        to
                    } else {
                        let c = if from < c { c - 1 } else { c };
                        if to <= c { c + 1 } else { c }
                    }
                });
            }
            &Command::RemoveFromQueue(at) if at < self.queue.len() && current != Some(at) => {
                self.queue.remove(at);
                self.playback.index = current.map(|c| if at < c { c - 1 } else { c });
            }
            Command::ClearUpcoming => {
                if let Some(c) = current {
                    self.queue.truncate(c + 1);
                }
            }
            Command::Equalizer(equalizer) => self.playback.equalizer = equalizer.clone(),
            &Command::Normalize(on) => self.playback.normalize = on,
            &Command::Mixes(mixes) => self.playback.mixes = mixes,
            &Command::SleepTimer(choice) => {
                self.playback.sleep = choice.map(|choice| crate::model::SleepTimer {
                    choice,
                    deadline: match choice {
                        crate::model::Sleep::Minutes(m) => {
                            Some(Instant::now() + Duration::from_secs(u64::from(m) * 60))
                        }
                        crate::model::Sleep::EndOfSong => None,
                    },
                });
            }
            _ => {}
        }
    }

    /// A page's first songs are resolved ahead while it shows: the first
    /// rows or cards of its first two shelves with songs.
    fn prepare_on_screen(&mut self) {
        if matches!(self.account, Account::Checking) || self.now_playing {
            return;
        }
        let ids: Vec<String> = match self
            .page_state(&self.view.target())
            .and_then(|s| s.page.as_ref())
        {
            Some(page) => page
                .shelves
                .iter()
                .map(|s| {
                    s.items
                        .iter()
                        .filter_map(|i| i.track.as_ref())
                        .take(4)
                        .map(|t| t.video_id.clone())
                        .collect::<Vec<_>>()
                })
                .filter(|ids| !ids.is_empty())
                .take(2)
                .flatten()
                .take(6)
                .collect(),
            None => return,
        };
        if ids.is_empty() || ids == self.on_screen {
            return;
        }
        self.on_screen = ids.clone();
        let unprepared: Vec<String> = ids
            .into_iter()
            .filter(|id| !self.backend.prepared(id))
            .collect();
        if !unprepared.is_empty() {
            self.backend.send(Command::PrepareMany(unprepared));
        }
    }

    /// Sends a suggestion request when the search text changed.
    pub fn search_changed(&mut self) {
        let input = self.search.trim().to_owned();
        if input == self.suggested_for {
            return;
        }
        self.suggested_for = input.clone();
        if input.chars().count() >= 2 {
            self.backend.send(Command::Suggest(input));
        } else {
            self.suggestions.clear();
        }
    }

    fn theme_frame(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.reload_themes) || self.themes.needs_reload() {
            self.start_themes();
        }
        if self.themes.poll()
            && let Some(theme) = self.themes.system_theme()
        {
            self.palette = theme.palette.clone();
        }
        if self.applied.as_ref() != Some(&self.palette) {
            // The desktop's first palette replaces the fallback at once;
            // only later theme changes are revealed.
            let first = !self.themed;
            if !first {
                self.transition.begin(ctx);
            }
            if first || !self.transition.holding(ctx) {
                crate::theme::apply(ctx, &self.palette);
                self.applied = Some(self.palette.clone());
                self.themed = true;
            }
        }
    }

    /// One frame of the open window.
    fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.background(&ctx);
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        self.desktop.focused.store(focused, Ordering::Relaxed);
        self.dropped(&ctx);
        self.prepare_on_screen();
        self.theme_frame(&ctx);
        self.lyrics_frame();
        self.cover_frame(&ctx);
        self.heat_frame();
        crate::derived::paint_frame(&ctx, self.paint_covers.then_some(&self.palette));

        #[cfg(feature = "e2e")]
        let registry = crate::e2e::take_registry(&ctx);
        let mut actions = Vec::new();
        match self.window {
            WindowKind::Main => crate::ui::draw(self, ui, &mut actions),
            WindowKind::Mini => crate::ui::mini::draw(self, ui, &mut actions),
        }
        for action in actions {
            self.apply(&ctx, action);
        }
        self.audition_frame(&ctx);
        self.transition.paint(&ctx);

        if self.first_frame.is_none() {
            self.first_frame = Some(self.started.elapsed());
            log::info!("first frame {:?} after start", self.started.elapsed());
        }
        #[cfg(feature = "e2e")]
        if let Some(mut driver) = self.e2e.take() {
            driver.frame(self, &ctx, registry);
            self.e2e = Some(driver);
        }
        // Keep the time display moving between backend updates.
        if self.playback.playing {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

/// Closing the window plays on: the window goes away, and the app (backend,
/// MPRIS, the command line) keeps running until it is shown again or quits.
/// Wayland can't hide a window, so it is closed and made again on demand.
impl fastframe_shell::Resident for App {
    fn closed(&self) -> fastframe_shell::Closed {
        use fastframe_shell::Closed;
        if self.quit_requested {
            Closed::Quit
        } else if self.switch_window {
            Closed::Reopen
        } else if !self.queue.is_empty() {
            Closed::Hide
        } else {
            Closed::Quit
        }
    }

    fn window_gone(&mut self) {
        // Nothing can be held under the pointer without a window.
        self.end_audition();
        log::info!("window closed; playing on in the background");
        self.hidden = true;
        self.switch_window = false;
        self.wants_show = false;
        self.desktop.focused.store(false, Ordering::Relaxed);
    }

    /// A tick every 150 ms while no window is open: no drawing, no repaints.
    fn headless_frame(&mut self, ctx: &egui::Context) -> fastframe_shell::Headless {
        use fastframe_shell::Headless;
        self.background(ctx);
        #[cfg(feature = "e2e")]
        if let Some(mut driver) = self.e2e.take() {
            driver.frame(self, ctx, Vec::new());
            self.e2e = Some(driver);
        }
        if self.quit_requested {
            Headless::Quit
        } else if self.wants_show {
            Headless::Show
        } else {
            Headless::Wait
        }
    }

    fn shutdown(&mut self) {
        // Save the session and stop mpv before the process goes, whatever
        // way fastframe-shell ends it; dropping the backend repeats it harmlessly.
        log::info!("quitting");
        self.backend.shutdown();
    }
}

/// The app as held by one native window.
pub struct Window(pub fastframe_shell::Held<App>);

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.frame(ui);
    }

    #[cfg(feature = "e2e")]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(driver) = &mut self.0.e2e {
            driver.inject(raw_input);
        }
    }
}
