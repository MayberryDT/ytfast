//! Application state. Views (in `ui`) read it and push [`Action`]s; the
//! app applies them after the frame and turns them into backend commands.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::backend::{Backend, Command, Event};
use crate::model::{Account, Lyrics, Page, Playback, Target, Track};
use crate::parse::More;
use crate::paths::Paths;
use crate::theme::Palette;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryTab {
    Playlists,
    Songs,
    Albums,
    Artists,
}

impl LibraryTab {
    pub const ALL: [LibraryTab; 4] = [
        LibraryTab::Playlists,
        LibraryTab::Songs,
        LibraryTab::Albums,
        LibraryTab::Artists,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LibraryTab::Playlists => "Playlists",
            LibraryTab::Songs => "Songs",
            LibraryTab::Albums => "Albums",
            LibraryTab::Artists => "Artists",
        }
    }

    pub fn target(self) -> Target {
        Target::browse(match self {
            LibraryTab::Playlists => "FEmusic_liked_playlists",
            LibraryTab::Songs => "FEmusic_liked_videos",
            LibraryTab::Albums => "FEmusic_liked_albums",
            LibraryTab::Artists => "FEmusic_library_corpus_track_artists",
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
    Lyrics(String),
    /// Forget a failed lyrics fetch so it is asked for again.
    RetryLyrics(String),
    DismissError(usize),
    Copy(String),
    /// The pointer rests on a song: resolve it ahead of a likely click.
    Prepare(String),
    /// Like the playing song, or remove its like.
    ToggleLikeCurrent,
    /// Likes, library, subscriptions and playlists (see `crate::account`).
    Account(crate::account::AccountAction),
}

pub struct App {
    pub backend: Backend,
    pub palette: Palette,
    applied: Option<Palette>,
    themes: fastframe_theme::Catalog<Palette>,
    transition: fastframe_theme::Transition,
    paths: Paths,
    show_requested: Arc<AtomicBool>,
    reload_themes: Arc<AtomicBool>,

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
    last_prepared: Option<String>,
    page_seq: u64,
    pub errors: Vec<String>,
    pub scroll_to_top: bool,
    pub started: Instant,
    /// How long the first frame took after launch.
    pub first_frame: Option<Duration>,
    #[cfg(feature = "e2e")]
    pub e2e: Option<crate::e2e::Driver>,
    /// The desktop's palette has been applied once; later changes animate.
    themed: bool,
    /// Likes, library and subscription marks, changes in flight, dialogs.
    pub account_state: crate::account::AccountState,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        backend: Backend,
        paths: Paths,
        show_requested: Arc<AtomicBool>,
        reload_themes: Arc<AtomicBool>,
        started: Instant,
    ) -> Self {
        let ctx = &cc.egui_ctx;
        let mut fonts = fastframe_fonts::FontSetup::default().definitions();
        fastframe_text::detect().apply_to(&mut fonts);
        ctx.set_fonts(fonts);
        egui_extras::install_image_loaders(ctx);
        fastframe_icons::install::<crate::icons::Icon>(ctx);
        ctx.add_bytes_loader(Arc::new(crate::covers::CoverLoader::new(
            backend.runtime.clone(),
            backend.http.clone(),
            paths.clone(),
        )));
        ctx.options_mut(|o| o.reduce_texture_memory = true);
        let palette = Palette::default();
        crate::theme::apply(ctx, &palette);

        let mut themes = fastframe_theme::Catalog::default();
        themes.enable_desktop_themes(fastframe_theme::DesktopThemes {
            slug: "ytfast",
            omarchy_template: fastframe_theme::omarchy::BASE_TEMPLATE,
            omarchy_previous_templates: &[],
            presets: false,
        });
        let mut app = Self {
            backend,
            palette: palette.clone(),
            applied: Some(palette),
            themes,
            transition: fastframe_theme::Transition::new(fastframe_theme::Reveal::Band),
            paths,
            show_requested,
            reload_themes,
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
        };
        app.start_themes(ctx);
        app.ensure_page(View::Home.target(), false);
        app.ensure_page(LibraryTab::Playlists.target(), false);
        app
    }

    fn start_themes(&mut self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        let waker = fastframe_theme::Waker::new(move || ctx.request_repaint());
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
        });
        state.loading = true;
        state.error = None;
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
                if !cached && self.pages.get(&key).is_some_and(|s| s.error.is_none()) {
                    self.account_page_fresh(&key);
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
            Event::Playback(playback) => self.playback = playback,
            Event::Error(error) => self.push_error(error),
            Event::Profiles { list, current } => {
                self.profiles = list;
                self.profile = current;
            }
            Event::AccountEdited { op, result } => self.account_edited(op, result),
            Event::Likes(likes) => self.account_likes(likes),
            Event::AccountRefresh(targets) => self.account_refresh(targets),
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
            Action::Command(command) => self.backend.send(command),
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
                if !query.is_empty() {
                    self.search = query.clone();
                    self.suggestions.clear();
                    self.open(View::Page(Target::Search {
                        query,
                        params: None,
                    }));
                }
            }
            Action::NowPlaying(open) => self.now_playing = open && !self.queue.is_empty(),
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
            Action::Lyrics(id) => {
                if self.lyrics_requested.insert(id.clone()) {
                    self.backend.send(Command::Lyrics(id));
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
            Action::ToggleLikeCurrent => self.toggle_like_current(),
            Action::Account(action) => self.account_action(action),
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
        if self.reload_themes.swap(false, Ordering::Relaxed) || self.themes.needs_reload() {
            self.start_themes(ctx);
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
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(event) = self.backend.events.try_recv() {
            self.handle(event);
        }
        if self.show_requested.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        self.theme_frame(&ctx);

        #[cfg(feature = "e2e")]
        let registry = crate::e2e::take_registry(&ctx);
        let mut actions = Vec::new();
        crate::ui::draw(self, ui, &mut actions);
        for action in actions {
            self.apply(&ctx, action);
        }
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

    #[cfg(feature = "e2e")]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(driver) = &mut self.e2e {
            driver.inject(raw_input);
        }
    }
}
