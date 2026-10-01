//! Play anything (Ctrl+K; notes/juice/signature-moments.md § Play
//! anything): one field over the library pages already loaded, recent
//! searches, the page on screen, YouTube Music's search and a few commands.
//!
//! Local results are worked out in the frame of the keystroke; YouTube
//! Music is asked ~150 ms after typing pauses and its answer joins in.
//! Ranking: exact and prefix title matches first, then the library over the
//! catalogue.

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use crate::app::{Action, App, LibraryTab};
use crate::backend::Command;
use crate::control::{ControlAction, FromPage};
use crate::equalizer::Preset;
use crate::model::{Item, ItemKind, Page, Sleep, Target, Track};

/// Typing pauses this long before YouTube Music is asked.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Results shown at once.
const SHOWN: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Song,
    Album,
    Playlist,
    Artist,
    Search,
    Command,
}

/// A command Play anything runs.
#[derive(Clone, Debug)]
pub enum Cmd {
    /// Radio from this song (the top song match of `radio <query>`).
    Radio(Box<Track>),
    Like,
    Next,
    Pause,
    Play,
    Shuffle,
    Repeat,
    Sleep(Option<Sleep>),
    Eq(Preset),
    Mini,
}

/// What choosing a result does.
#[derive(Clone, Debug)]
pub enum Go {
    /// Play it, with the song results as the queue; Shift opens its album.
    Song(Track),
    /// Play an album or playlist; Shift opens its page.
    Collection {
        page: Option<Target>,
        play: Option<Target>,
    },
    Artist(Target),
    Search(String),
    Command(Cmd),
    /// Put this in the field: a command waiting for what follows it.
    Complete(String),
}

#[derive(Clone, Debug)]
pub struct Hit {
    /// Who it is, across frames (results glide when they move).
    pub key: String,
    pub title: String,
    pub detail: String,
    pub thumbnail: Option<String>,
    pub kind: Kind,
    pub go: Go,
}

/// What choosing a result asks of the interface.
pub enum Chosen {
    Actions(Vec<Action>),
    /// Replace the text in the field.
    Complete(String),
}

pub struct PlayAnything {
    pub open: bool,
    pub query: String,
    /// The highlighted result.
    pub selected: usize,
    /// When the query last changed.
    changed: Instant,
    /// The catalogue search last asked for.
    asked: String,
    /// YouTube Music's last answer: its query and results (or what went wrong).
    catalogue: Option<(String, Result<Page, String>)>,
    generation: u64,
    hits: Vec<Hit>,
    /// What `hits` were worked out from.
    stamp: u64,
}

impl Default for PlayAnything {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            selected: 0,
            changed: Instant::now(),
            asked: String::new(),
            catalogue: None,
            generation: 0,
            hits: Vec::new(),
            stamp: 0,
        }
    }
}

impl PlayAnything {
    /// Opens empty, or closes.
    pub fn set_open(&mut self, open: bool) {
        if open && !self.open {
            self.query.clear();
            self.selected = 0;
            self.asked.clear();
            self.catalogue = None;
            self.stamp = 0;
        }
        self.open = open;
    }

    /// The field changed: the top result is highlighted again.
    pub fn edited(&mut self) {
        self.changed = Instant::now();
        self.selected = 0;
    }

    /// YouTube Music hasn't answered for what's typed yet.
    pub fn searching(&self) -> bool {
        catalogue_query(&self.query).is_some_and(|q| {
            self.catalogue
                .as_ref()
                .is_none_or(|(answered, _)| *answered != q)
        })
    }

    /// YouTube Music couldn't be asked.
    pub fn failed(&self) -> bool {
        matches!(&self.catalogue, Some((_, Err(_))))
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// What choosing result `index` does; `open` (Shift+Enter) opens its
    /// page instead of playing it.
    pub fn choose(&self, app: &App, index: usize, open: bool) -> Option<Chosen> {
        let hit = self.hits.get(index)?;
        let actions = match &hit.go {
            Go::Song(track) => {
                let album = track.album.as_ref().and_then(|a| a.target.clone());
                match (open, album) {
                    (true, Some(album)) => vec![Action::Activate(album)],
                    _ => {
                        let tracks: Vec<Track> = self
                            .hits
                            .iter()
                            .filter_map(|h| match &h.go {
                                Go::Song(t) => Some(t.clone()),
                                _ => None,
                            })
                            .collect();
                        let start = tracks
                            .iter()
                            .position(|t| t.video_id == track.video_id)
                            .unwrap_or(0);
                        vec![Action::Play { tracks, start }]
                    }
                }
            }
            Go::Collection { page, play } => match (page, play) {
                (Some(page), _) if open => vec![Action::Activate(page.clone())],
                (_, Some(play)) => vec![Action::Activate(play.clone())],
                (Some(page), None) => vec![Action::Control(ControlAction::FromPage {
                    page: page.clone(),
                    how: FromPage::Play,
                })],
                (None, None) => return None,
            },
            Go::Artist(target) => vec![Action::Activate(target.clone())],
            Go::Search(query) => vec![Action::Search(query.clone())],
            Go::Complete(text) => return Some(Chosen::Complete(text.clone())),
            Go::Command(cmd) => command_actions(app, cmd),
        };
        Some(Chosen::Actions(actions))
    }
}

fn command_actions(app: &App, cmd: &Cmd) -> Vec<Action> {
    let command = |c| vec![Action::Command(c)];
    let playing = app.playback.playing;
    match cmd {
        Cmd::Radio(track) => command(Command::PlayTarget(Target::Watch {
            video_id: Some(track.video_id.clone()),
            playlist_id: Some(format!("RDAMVM{}", track.video_id)),
            params: Some("wAEB".into()),
        })),
        Cmd::Like => vec![Action::ToggleLikeCurrent],
        Cmd::Next => command(Command::Next),
        Cmd::Pause if playing => command(Command::TogglePause),
        Cmd::Play if !playing && !app.queue.is_empty() => command(Command::TogglePause),
        Cmd::Pause | Cmd::Play => Vec::new(),
        Cmd::Shuffle => command(Command::ToggleShuffle),
        Cmd::Repeat => command(Command::CycleRepeat),
        Cmd::Sleep(choice) => command(Command::SleepTimer(*choice)),
        Cmd::Eq(preset) => command(Command::Equalizer(
            app.playback.equalizer.with_preset(*preset),
        )),
        Cmd::Mini => vec![Action::MiniPlayer(true)],
    }
}

/// What YouTube Music is searched for: the words after `radio`, nothing
/// for `sleep …` and `eq …`, else the whole text (two letters or more).
fn catalogue_query(query: &str) -> Option<String> {
    let query = query.trim();
    let (word, rest) = split_command(query);
    let wanted = match word.as_str() {
        "radio" if !rest.is_empty() => rest,
        "sleep" | "eq" if !rest.is_empty() => return None,
        _ => query,
    };
    (wanted.chars().count() >= 2).then(|| wanted.to_owned())
}

/// The first word, lowercased, and the rest.
fn split_command(query: &str) -> (String, &str) {
    let (word, rest) = query.split_once(char::is_whitespace).unwrap_or((query, ""));
    (word.to_lowercase(), rest.trim())
}

/// Where a result comes from, in the order they rank among equals.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Command,
    Library,
    Page,
    Recent,
    Catalogue,
    /// A command whose name starts with what's typed: offered after the music.
    Suggestion,
}

struct Ranked {
    hit: Hit,
    source: Source,
    /// 0 exact title, 1 prefix, 2 word prefix, 3 inside, 4 every word
    /// somewhere, 5 only YouTube Music's word for it.
    tier: u8,
    order: usize,
}

fn tier(title: &str, detail: &str, q: &str) -> Option<u8> {
    let title = title.to_lowercase();
    if title == q {
        return Some(0);
    }
    if title.starts_with(q) {
        return Some(1);
    }
    if title.contains(q) {
        let starts_word = title.match_indices(q).any(|(i, _)| {
            title[..i]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric())
        });
        return Some(if starts_word { 2 } else { 3 });
    }
    let all = format!("{title} {}", detail.to_lowercase());
    q.split_whitespace()
        .all(|word| all.contains(word))
        .then_some(4)
}

/// A result for a card or row, if it is music.
fn hit_for(item: &Item) -> Option<Hit> {
    let detail: String = item.subtitle.iter().map(|r| r.text.as_str()).collect();
    if let Some(track) = &item.track {
        let artists = track.artist_line();
        return Some(Hit {
            key: format!("song:{}", track.video_id),
            title: item.title.clone(),
            detail: if artists.is_empty() { detail } else { artists },
            thumbnail: item.thumbnail.clone().or_else(|| track.thumbnail.clone()),
            kind: Kind::Song,
            go: Go::Song(track.clone()),
        });
    }
    let target = item.target.clone();
    let (kind, go) = match item.kind {
        ItemKind::Album | ItemKind::Playlist => (
            if item.kind == ItemKind::Album {
                Kind::Album
            } else {
                Kind::Playlist
            },
            Go::Collection {
                page: target
                    .clone()
                    .filter(|t| matches!(t, Target::Browse { .. })),
                play: item
                    .play
                    .clone()
                    .or_else(|| target.clone().filter(|t| matches!(t, Target::Watch { .. }))),
            },
        ),
        ItemKind::Artist => (Kind::Artist, Go::Artist(target.clone()?)),
        _ => return None,
    };
    Some(Hit {
        key: format!("page:{}", target?.key()),
        title: item.title.clone(),
        detail,
        thumbnail: item.thumbnail.clone(),
        kind,
        go,
    })
}

fn command_hit(key: &str, title: impl Into<String>, go: Go) -> Hit {
    Hit {
        key: format!("command:{key}"),
        title: title.into(),
        detail: "Command".into(),
        thumbnail: None,
        kind: Kind::Command,
        go,
    }
}

const SLEEP: [(&str, &str, Option<Sleep>); 6] = [
    ("15", "Sleep in 15 minutes", Some(Sleep::Minutes(15))),
    ("30", "Sleep in 30 minutes", Some(Sleep::Minutes(30))),
    ("45", "Sleep in 45 minutes", Some(Sleep::Minutes(45))),
    ("60", "Sleep in 1 hour", Some(Sleep::Minutes(60))),
    (
        "end",
        "Sleep at the end of the song",
        Some(Sleep::EndOfSong),
    ),
    ("off", "Turn the sleep timer off", None),
];

/// Commands for what's typed: exact ones (and their arguments) rank first,
/// ones the text only begins are offered after the music.
fn commands(query: &str, out: &mut Vec<Ranked>) {
    let (word, rest) = split_command(query);
    let mut push = |hit: Hit, source: Source, tier: u8| {
        let order = out.len();
        out.push(Ranked {
            hit,
            source,
            tier,
            order,
        });
    };
    match word.as_str() {
        "sleep" => {
            let rest = rest.to_lowercase();
            if let Ok(minutes) = rest.parse::<u32>()
                && minutes > 0
            {
                push(
                    command_hit(
                        &format!("sleep-{minutes}"),
                        format!(
                            "Sleep in {minutes} minute{}",
                            if minutes == 1 { "" } else { "s" }
                        ),
                        Go::Command(Cmd::Sleep(Some(Sleep::Minutes(minutes)))),
                    ),
                    Source::Command,
                    0,
                );
            } else {
                for (name, title, choice) in SLEEP {
                    if name.starts_with(&rest) {
                        push(
                            command_hit(
                                &format!("sleep-{name}"),
                                title,
                                Go::Command(Cmd::Sleep(choice)),
                            ),
                            Source::Command,
                            0,
                        );
                    }
                }
            }
            return;
        }
        "eq" => {
            let rest = rest.to_lowercase();
            for preset in Preset::ALL {
                let label = preset.label().to_lowercase();
                if rest.is_empty() || label.starts_with(&rest) || label.contains(&rest) {
                    push(
                        command_hit(
                            &format!("eq-{label}"),
                            format!("Equalizer: {}", preset.label()),
                            Go::Command(Cmd::Eq(preset)),
                        ),
                        Source::Command,
                        0,
                    );
                }
            }
            return;
        }
        _ => {}
    }
    let simple: [(&str, &str, Cmd); 7] = [
        ("like", "Like or unlike the playing song", Cmd::Like),
        ("next", "Next song", Cmd::Next),
        ("pause", "Pause", Cmd::Pause),
        ("play", "Play", Cmd::Play),
        ("shuffle", "Shuffle on or off", Cmd::Shuffle),
        ("repeat", "Repeat: off, all or one", Cmd::Repeat),
        ("mini", "Mini player", Cmd::Mini),
    ];
    let single = rest.is_empty();
    for (name, title, cmd) in simple {
        if single && word == name {
            push(
                command_hit(name, title, Go::Command(cmd)),
                Source::Command,
                0,
            );
        } else if single && word.chars().count() >= 2 && name.starts_with(&word) {
            push(
                command_hit(name, title, Go::Command(cmd)),
                Source::Suggestion,
                1,
            );
        }
    }
    if single && word.chars().count() >= 2 {
        for (name, title) in [
            ("radio", "Start a radio: radio <song or artist>"),
            ("sleep", "Sleep timer: sleep <minutes> or sleep end"),
            ("eq", "Equalizer preset: eq <preset>"),
        ] {
            if name.starts_with(&word) {
                let tier = if word == name { 0 } else { 1 };
                let source = if tier == 0 {
                    Source::Command
                } else {
                    Source::Suggestion
                };
                push(
                    command_hit(name, title, Go::Complete(format!("{name} "))),
                    source,
                    tier,
                );
            }
        }
    }
}

/// The library pages loaded so far: Library's sections and Liked Music.
fn library_targets() -> impl Iterator<Item = Target> {
    LibraryTab::ALL
        .into_iter()
        .filter(|t| *t != LibraryTab::History)
        .map(LibraryTab::target)
        .chain(std::iter::once(Target::browse("VLLM")))
}

fn page_items(page: &Page) -> impl Iterator<Item = &Item> {
    page.shelves.iter().flat_map(|s| &s.items)
}

/// The results for `query`, best first.
fn rank(app: &App, pa: &PlayAnything) -> Vec<Hit> {
    let query = pa.query.trim();
    if query.is_empty() {
        return app
            .recent_searches
            .iter()
            .take(5)
            .map(|q| Hit {
                key: format!("search:{q}"),
                title: q.clone(),
                detail: "Recent search".into(),
                thumbnail: None,
                kind: Kind::Search,
                go: Go::Search(q.clone()),
            })
            .collect();
    }
    let mut ranked: Vec<Ranked> = Vec::new();
    commands(query, &mut ranked);
    let (word, rest) = split_command(query);
    let radio = word == "radio" && !rest.is_empty();
    if (word == "sleep" || word == "eq") && !rest.is_empty() {
        return ranked.into_iter().map(|r| r.hit).collect();
    }
    let term = if radio { rest } else { query }.to_lowercase();

    let add = |item: &Item, source: Source, ranked: &mut Vec<Ranked>| {
        let Some(hit) = hit_for(item) else { return };
        let tier = match tier(&hit.title, &hit.detail, &term) {
            Some(t) => t,
            None if source == Source::Catalogue => 5,
            None => return,
        };
        let order = ranked.len();
        ranked.push(Ranked {
            hit,
            source,
            tier,
            order,
        });
    };
    let view = app.view.target();
    let mut library_keys = Vec::new();
    for target in library_targets() {
        library_keys.push(target.key());
        if let Some(page) = app.page_state(&target).and_then(|s| s.page.as_ref()) {
            // Liked Music's own list, not the Suggestions after it.
            let items: Vec<&Item> = match crate::account::entries(page) {
                Some(own) if target == Target::browse("VLLM") => own.items.iter().collect(),
                _ => page_items(page).collect(),
            };
            for item in items {
                add(item, Source::Library, &mut ranked);
            }
        }
    }
    if !library_keys.contains(&view.key())
        && let Some(page) = app.page_state(&view).and_then(|s| s.page.as_ref())
    {
        for item in page_items(page) {
            add(item, Source::Page, &mut ranked);
        }
    }
    if !radio {
        for q in &app.recent_searches {
            if let Some(tier) = tier(q, "", &term).filter(|t| *t <= 2) {
                let order = ranked.len();
                ranked.push(Ranked {
                    hit: Hit {
                        key: format!("search:{q}"),
                        title: q.clone(),
                        detail: "Recent search".into(),
                        thumbnail: None,
                        kind: Kind::Search,
                        go: Go::Search(q.clone()),
                    },
                    source: Source::Recent,
                    tier,
                    order,
                });
            }
        }
    }
    if let (Some((answered, Ok(page))), Some(wanted)) = (&pa.catalogue, catalogue_query(query))
        && (wanted.starts_with(answered.as_str()) || answered.starts_with(&wanted))
    {
        // YouTube Music's best: the top result, then a few of each kind.
        let mut taken = [0usize; 4];
        for shelf in &page.shelves {
            for item in &shelf.items {
                let slot = match (item.track.is_some(), item.kind) {
                    (true, _) => 0,
                    (false, ItemKind::Album) => 1,
                    (false, ItemKind::Artist) => 2,
                    (false, ItemKind::Playlist) => 3,
                    _ => continue,
                };
                if taken[slot] < [6, 3, 2, 2][slot] {
                    taken[slot] += 1;
                    add(item, Source::Catalogue, &mut ranked);
                }
            }
        }
    }
    // Exact and prefix title matches first, then by source, then closeness.
    ranked.sort_by_key(|r| (r.tier > 1, r.source, r.tier, r.order));
    let mut seen = std::collections::HashSet::new();
    let mut hits: Vec<Hit> = ranked
        .into_iter()
        .filter(|r| seen.insert(r.hit.key.clone()))
        .map(|r| r.hit)
        .collect();
    if radio {
        hits.retain(|h| h.kind == Kind::Song);
        if let Some(Go::Song(track)) = hits.first().map(|h| h.go.clone()) {
            let artists = track.artist_line();
            hits.insert(
                0,
                Hit {
                    key: format!("radio:{}", track.video_id),
                    title: format!("Start radio from “{}”", track.title),
                    detail: artists,
                    thumbnail: track.thumbnail.clone(),
                    kind: Kind::Command,
                    go: Go::Command(Cmd::Radio(Box::new(track))),
                },
            );
        }
    }
    hits.truncate(SHOWN);
    hits
}

impl App {
    /// Works Play anything's results out again when what's typed or what
    /// they come from changed.
    pub(crate) fn refresh_hits(&mut self) {
        let stamp = self.hits_stamp();
        if stamp == self.control.play_anything.stamp {
            return;
        }
        let hits = rank(self, &self.control.play_anything);
        let pa = &mut self.control.play_anything;
        pa.hits = hits;
        pa.stamp = stamp;
        pa.selected = pa.selected.min(pa.hits.len().saturating_sub(1));
    }

    /// Changes whenever the results could: the text, YouTube Music's
    /// answer, the library and the page on screen.
    fn hits_stamp(&self) -> u64 {
        let pa = &self.control.play_anything;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        pa.query.hash(&mut h);
        pa.generation.hash(&mut h);
        self.recent_searches.len().hash(&mut h);
        let view = self.view.target();
        view.key().hash(&mut h);
        for target in library_targets().chain(std::iter::once(view)) {
            if let Some(state) = self.page_state(&target) {
                state.fetched.hash(&mut h);
                state
                    .page
                    .as_ref()
                    .map(|p| p.shelves.iter().map(|s| s.items.len()).sum::<usize>())
                    .hash(&mut h);
            }
        }
        h.finish() | 1
    }

    /// Asks YouTube Music once typing pauses.
    pub(crate) fn play_anything_frame(&mut self, ctx: &egui::Context) {
        let pa = &mut self.control.play_anything;
        if !pa.open {
            return;
        }
        let Some(query) = catalogue_query(&pa.query) else {
            return;
        };
        if query == pa.asked {
            return;
        }
        let waited = pa.changed.elapsed();
        if waited >= DEBOUNCE {
            pa.asked = query.clone();
            self.backend.send(Command::QuickSearch(query));
        } else {
            ctx.request_repaint_after(DEBOUNCE - waited);
        }
    }

    /// YouTube Music's answer; only the one for the latest search is kept.
    pub(crate) fn quick_results(&mut self, query: String, result: Result<Box<Page>, String>) {
        let pa = &mut self.control.play_anything;
        if query != pa.asked {
            return;
        }
        pa.catalogue = Some((query, result.map(|page| *page)));
        pa.generation += 1;
    }
}
