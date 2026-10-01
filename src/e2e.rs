//! The scripted end-to-end run (cargo feature `e2e`; see
//! `scripts/e2e.sh`). It drives the real app, with the real backend, account,
//! network and mpv, through the journeys in docs/SPEC.md § Completion
//! evidence. Input goes through egui's own pipeline: controls are found by
//! the names they give screen readers (`ui::named`) and clicked with
//! synthetic pointer events. Every step is logged, screenshots are taken from
//! the real framebuffer, and `summary.json` records measurements and
//! failures, in the directory named by `YTFAST_E2E_DIR`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Event, Id, Pos2, Rect};
use serde_json::{Value, json};

use crate::app::{App, LibraryTab, View};
use crate::backend::Command;
use crate::model::{Account, Target};

/// Streams to break on purpose (video id → how many more resolves fail), for
/// the recovery scenario.
static SABOTAGE: std::sync::Mutex<BTreeMap<String, u32>> = std::sync::Mutex::new(BTreeMap::new());

fn sabotage(video_id: &str, times: u32) {
    SABOTAGE
        .lock()
        .expect("sabotage lock")
        .insert(video_id.to_owned(), times);
}

/// Whether the resolver should hand out a broken stream for `video_id` now.
pub fn sabotaged(video_id: &str) -> bool {
    let mut map = SABOTAGE.lock().expect("sabotage lock");
    match map.get_mut(video_id) {
        Some(n) if *n > 0 => {
            *n -= 1;
            true
        }
        _ => false,
    }
}

/// Simulated loss of connection: streams fail and YouTube reads as unreachable.
static OFFLINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn offline() -> bool {
    OFFLINE.load(std::sync::atomic::Ordering::Relaxed)
}

fn set_offline(on: bool) {
    OFFLINE.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn page_tracks(app: &App) -> Vec<String> {
    app.page_state(&app.view.target())
        .and_then(|s| s.page.as_ref())
        .map(|p| {
            p.shelves
                .iter()
                .flat_map(|s| &s.items)
                .filter_map(|i| i.track.as_ref())
                .map(|t| t.video_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn scenario(name: &str) -> Vec<Step> {
    match name {
        "recovery" => recovery(),
        "offline" => no_connection(),
        "theme" => theme(),
        "showcase" => showcase(),
        "account" => account(),
        _ => journey(),
    }
}

/// An expired browser session, then Reconnect once the browser has a valid
/// one; a stream that fails once (retried), one that keeps failing (skipped
/// with an error), and a lost connection mid-queue (playback waits, then
/// resumes). Runs with a home directory holding a copy of the browser
/// profile whose session cookies were invalidated (scripts/e2e.sh); the
/// reconnect step swaps in the real profile.
fn recovery() -> Vec<Step> {
    let home = View::Home.target();
    vec![
        wait("signed out", 60.0, |a| {
            matches!(a.account, Account::SignedOut { .. })
        }),
        measure("signed_out_reason", |a| match &a.account {
            Account::SignedOut { reason } => json!(reason),
            _ => Value::Null,
        }),
        wait("public home", 60.0, move |a| loaded(a, &home, 1)),
        Step::Sleep(4.0),
        Step::Screenshot("r1-signed-out"),
        run("let the browser profile appear", |_| {
            let (Some(real), Some(fake)) = (
                std::env::var_os("YTFAST_E2E_REAL_HOME"),
                std::env::var_os("HOME"),
            ) else {
                return;
            };
            let browser =
                std::env::var("YTFAST_E2E_BROWSER").unwrap_or_else(|_| "google-chrome".into());
            let link = PathBuf::from(fake).join(".config").join(&browser);
            let _ = std::fs::remove_dir_all(&link);
            let _ = std::fs::create_dir_all(link.parent().expect("parent"));
            let _ = std::os::unix::fs::symlink(
                PathBuf::from(real).join(".config").join(&browser),
                link,
            );
        }),
        click("Reconnect"),
        wait("signed in again", 60.0, |a| {
            matches!(a.account, Account::SignedIn { .. })
        }),
        wait("account home", 60.0, |a| loaded(a, &View::Home.target(), 2)),
        Step::Sleep(4.0),
        Step::Screenshot("r2-reconnected"),
        wait("library playlists", 60.0, |a| library_playlist(a).is_some()),
        click_with("a library playlist with 8+ songs", library_playlist),
        wait("playlist page", 60.0, current_loaded),
        run("break song 1 once and song 2 for good", |a| {
            let tracks = page_tracks(a);
            if let [first, second, ..] = tracks.as_slice() {
                sabotage(first, 1);
                // Prefetches use up resolves too; keep song 2 broken through every attempt.
                sabotage(second, 10);
            }
        }),
        click("Play"),
        wait("song 1 plays after a retry", 90.0, |a| {
            a.playback.index == Some(0) && a.playback.playing && a.playback.position > 0.5
        }),
        measure("song_1", playing_track),
        wait("song 2 queued", 60.0, |a| a.playback.next_ready),
        run("seek near the end of song 1", |a| {
            let end = (a.playback.duration - 3.0).max(0.0);
            a.backend.send(Command::Seek(end));
        }),
        wait("song 2 skipped, song 3 playing", 120.0, |a| {
            a.playback.index == Some(2) && a.playback.playing && !a.errors.is_empty()
        }),
        measure("errors", |a| json!(a.errors)),
        measure("song_3", playing_track),
        Step::Sleep(1.0),
        Step::Screenshot("r3-skipped"),
        // The connection drops: the next song can't load, and playback waits
        // instead of skipping through the queue; then it comes back.
        run("lose the connection and jump ahead", |a| {
            set_offline(true);
            a.backend.send(Command::JumpTo(4));
        }),
        wait("waiting for the connection", 90.0, |a| {
            a.playback.index == Some(4)
                && a.playback.loading
                && a.errors.iter().any(|e| e.starts_with("No connection"))
        }),
        Step::Sleep(6.0),
        measure(
            "while_offline",
            |a| json!({"index": a.playback.index, "playing": a.playback.playing, "errors": a.errors}),
        ),
        Step::Screenshot("r4-waiting-for-connection"),
        run("the connection is back", |_| set_offline(false)),
        wait("resumed the same song", 90.0, |a| {
            a.playback.index == Some(4) && a.playback.playing && a.playback.position > 0.5
        }),
        measure("resumed", playing_track),
        run("pause", |a| {
            if a.playback.playing {
                a.backend.send(Command::TogglePause);
            }
        }),
    ]
}

/// No connection (a dead proxy for every request): the saved copies show,
/// with a note and Retry, and the account reads as offline.
fn no_connection() -> Vec<Step> {
    let home = View::Home.target();
    vec![
        wait("offline account", 60.0, |a| {
            matches!(a.account, Account::Unverified { .. })
        }),
        measure("account", |a| match &a.account {
            Account::Unverified { reason } => json!(reason),
            _ => Value::Null,
        }),
        wait("home from the saved copy", 60.0, move |a| {
            a.page_state(&home)
                .is_some_and(|s| s.page.is_some() && s.error.is_some() && !s.loading)
        }),
        measure("home_error", |a| {
            json!(
                a.page_state(&View::Home.target())
                    .and_then(|s| s.error.clone())
            )
        }),
        Step::Sleep(4.0),
        Step::Screenshot("o1-offline-home"),
        click("Explore"),
        wait("explore settles", 60.0, |a| {
            a.page_state(&View::Explore.target())
                .is_some_and(|s| !s.loading)
        }),
        Step::Sleep(2.0),
        Step::Screenshot("o2-offline-explore"),
    ]
}

fn omarchy_theme() -> Option<String> {
    let out = std::process::Command::new("omarchy-theme-current")
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned()).filter(|s| !s.is_empty())
}

/// Switches the desktop theme on a separate thread: `omarchy-theme-set` can
/// take minutes on a slow machine, and the window must keep drawing. When
/// the app runs with a stand-in home, the switch happens in the real one.
fn set_theme(name: &str) {
    let name = name.to_owned();
    std::thread::spawn(move || {
        let mut command = std::process::Command::new("omarchy-theme-set");
        command.arg(name);
        if let Some(home) = std::env::var_os("YTFAST_E2E_REAL_HOME") {
            let home = PathBuf::from(home);
            command
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .env("XDG_CACHE_HOME", home.join(".cache"))
                .env("XDG_STATE_HOME", home.join(".local/state"))
                .env("HOME", home);
        }
        let _ = command.status();
    });
}

/// The desktop's theme changes while ytfast runs: colours follow live.
/// Switches to `YTFAST_E2E_LIGHT_THEME` (default "Snow") and back.
fn theme() -> Vec<Step> {
    let home = View::Home.target();
    let original = omarchy_theme().unwrap_or_else(|| "Permafrost".into());
    let light = std::env::var("YTFAST_E2E_LIGHT_THEME").unwrap_or_else(|_| "Snow".into());
    let (o1, o2) = (original.clone(), original.clone());
    vec![
        wait("home loaded", 60.0, move |a| loaded(a, &home, 1)),
        Step::Sleep(4.0),
        measure(
            "original_theme",
            move |a| json!({"name": o1, "dark": a.palette.dark, "window": format!("{:?}", a.palette.window)}),
        ),
        Step::Screenshot("t1-original"),
        run("switch the desktop theme", move |_| set_theme(&light)),
        wait("colours changed", 60.0, |a| !a.palette.dark),
        Step::Sleep(3.0),
        measure(
            "light_theme",
            |a| json!({"dark": a.palette.dark, "window": format!("{:?}", a.palette.window)}),
        ),
        Step::Screenshot("t2-light"),
        run("switch back", move |_| set_theme(&o2)),
        wait("colours back", 60.0, |a| a.palette.dark),
        Step::Sleep(3.0),
        Step::Screenshot("t3-back"),
    ]
}

/// Pictures for the README, with no account: run with an empty home
/// directory (scripts/e2e.sh), so ytfast is signed out and shows public
/// YouTube Music. Home, a search, an artist, an album playing, Now Playing,
/// and the same album in a light theme (switched back afterwards).
fn showcase() -> Vec<Step> {
    let home = View::Home.target();
    let original = omarchy_theme().unwrap_or_else(|| "Stakeout".into());
    let light = std::env::var("YTFAST_E2E_LIGHT_THEME").unwrap_or_else(|_| "Snow".into());
    vec![
        wait("signed out", 60.0, |a| {
            matches!(a.account, Account::SignedOut { .. })
        }),
        wait("public home", 60.0, move |a| loaded(a, &home, 1)),
        Step::Sleep(5.0),
        Step::Screenshot("home"),
        click("Search"),
        Step::Type("Daft Punk".into()),
        Step::Key(egui::Key::Enter),
        wait("search results", 60.0, |a| {
            matches!(&a.view, View::Page(Target::Search { .. })) && current_loaded(a)
        }),
        Step::Sleep(4.0),
        Step::Screenshot("search"),
        // Opened directly: a playlist on the results has the artist's name too.
        run("open the artist", |a| {
            open_item(a, |i| i.kind == crate::model::ItemKind::Artist);
        }),
        wait("artist page", 60.0, artist_or_album),
        // A top song, not the album's Play: signed out, an album plays as its
        // music videos, whose covers are video frames.
        click_with("a top song", |a| {
            first_item_title(a, &a.view.target(), |i| {
                i.kind == crate::model::ItemKind::Song
            })
        }),
        wait("playing", 90.0, |a| {
            a.playback.playing && a.playback.position > 0.5
        }),
        run("seek a third in", |a| {
            a.backend.send(Command::Seek(a.playback.duration * 0.35));
        }),
        Step::Sleep(5.0),
        Step::Screenshot("artist"),
        run("open an album", |a| {
            let album = |i: &crate::model::Item| i.kind == crate::model::ItemKind::Album;
            if !open_item(a, |i| album(i) && i.title == "Random Access Memories") {
                open_item(a, album);
            }
        }),
        wait("album page", 60.0, artist_or_album),
        Step::Sleep(4.0),
        Step::Screenshot("album"),
        click("Open player"),
        wait("now playing", 10.0, |a| a.now_playing),
        Step::Sleep(3.0),
        Step::Screenshot("now-playing"),
        click("LYRICS"),
        wait("lyrics", 30.0, |a| {
            a.playback
                .lyrics
                .as_ref()
                .is_some_and(|id| matches!(a.lyrics.get(id), Some(Ok(Some(_)))))
        }),
        Step::Sleep(2.0),
        Step::Screenshot("lyrics"),
        click("Close player"),
        run("switch to a light theme", move |_| set_theme(&light)),
        wait("light colours", 120.0, |a| !a.palette.dark),
        Step::Sleep(4.0),
        Step::Screenshot("album-light-theme"),
        run("switch the theme back", move |_| set_theme(&original)),
        wait("dark colours", 120.0, |a| a.palette.dark),
        click("Explore"),
        wait("explore", 60.0, |a| loaded(a, &View::Explore.target(), 2)),
        Step::Sleep(4.0),
        Step::Screenshot("explore"),
        run("pause", |a| {
            if a.playback.playing {
                a.backend.send(Command::TogglePause);
            }
        }),
    ]
}

/// Opens the first item on the current page that `pick` accepts, as a click
/// on it would; false if there is none.
fn open_item(app: &mut App, pick: impl Fn(&crate::model::Item) -> bool) -> bool {
    let target = app
        .page_state(&app.view.target())
        .and_then(|s| s.page.as_ref())
        .and_then(|p| {
            p.shelves
                .iter()
                .flat_map(|s| &s.items)
                .find(|i| pick(i))
                .and_then(|i| i.target.clone())
        });
    match target {
        Some(target) => {
            app.open(View::Page(target));
            true
        }
        None => false,
    }
}

/// A loaded browse page (not the search results it was opened from).
fn artist_or_album(app: &App) -> bool {
    current_loaded(app) && matches!(&app.view, View::Page(t) if !matches!(t, Target::Search { .. }))
}

#[derive(Clone, Default)]
struct Registry(Vec<(String, Rect)>);

fn registry_id() -> Id {
    Id::new("ytfast-e2e-registry")
}

/// Records a named control's visible (clipped) area for this frame.
pub fn register(ctx: &egui::Context, label: &str, rect: Rect) {
    if rect.width() >= 4.0 && rect.height() >= 4.0 {
        ctx.data_mut(|d| {
            d.get_temp_mut_or_default::<Registry>(registry_id())
                .0
                .push((label.to_owned(), rect))
        });
    }
}

/// The controls named during the previous frame.
pub fn take_registry(ctx: &egui::Context) -> Vec<(String, Rect)> {
    ctx.data_mut(|d| std::mem::take(&mut d.get_temp_mut_or_default::<Registry>(registry_id()).0))
}

type Check = Box<dyn Fn(&App) -> bool>;
type Label = Box<dyn Fn(&App) -> Option<String>>;
type Measure = Box<dyn Fn(&App) -> Value>;
type Run = Box<dyn Fn(&mut App)>;

enum Step {
    Wait {
        what: &'static str,
        timeout: f64,
        check: Check,
    },
    Click {
        label: Label,
        describe: String,
        timeout: f64,
    },
    Type(String),
    Key(egui::Key),
    Screenshot(&'static str),
    Measure {
        name: &'static str,
        value: Measure,
    },
    Sleep(f64),
    Run(&'static str, Run),
    /// Rests the pointer on a named control (hover-only controls appear).
    Hover {
        label: Label,
        describe: String,
    },
    /// Presses on one named control, moves over several frames and
    /// releases on another.
    Drag {
        from: Label,
        to: Label,
        describe: String,
    },
    /// Runs `refresh` every `every` seconds until `check` holds (YouTube
    /// Music shows some account changes only after a few seconds).
    Poll {
        what: &'static str,
        timeout: f64,
        every: f64,
        refresh: Run,
        check: Check,
    },
}

fn wait(what: &'static str, timeout: f64, check: impl Fn(&App) -> bool + 'static) -> Step {
    Step::Wait {
        what,
        timeout,
        check: Box::new(check),
    }
}

fn click(label: &str) -> Step {
    let fixed = label.to_owned();
    Step::Click {
        describe: fixed.clone(),
        label: Box::new(move |_| Some(fixed.clone())),
        timeout: 15.0,
    }
}

fn click_with(describe: &str, label: impl Fn(&App) -> Option<String> + 'static) -> Step {
    Step::Click {
        describe: describe.to_owned(),
        label: Box::new(label),
        timeout: 20.0,
    }
}

fn measure(name: &'static str, value: impl Fn(&App) -> Value + 'static) -> Step {
    Step::Measure {
        name,
        value: Box::new(value),
    }
}

fn run(what: &'static str, f: impl Fn(&mut App) + 'static) -> Step {
    Step::Run(what, Box::new(f))
}

fn hover_with(describe: &str, label: impl Fn(&App) -> Option<String> + 'static) -> Step {
    Step::Hover {
        describe: describe.to_owned(),
        label: Box::new(label),
    }
}

fn drag_with(
    describe: &str,
    from: impl Fn(&App) -> Option<String> + 'static,
    to: impl Fn(&App) -> Option<String> + 'static,
) -> Step {
    Step::Drag {
        describe: describe.to_owned(),
        from: Box::new(from),
        to: Box::new(to),
    }
}

fn poll(
    what: &'static str,
    timeout: f64,
    refresh: impl Fn(&mut App) + 'static,
    check: impl Fn(&App) -> bool + 'static,
) -> Step {
    Step::Poll {
        what,
        timeout,
        every: 3.0,
        refresh: Box::new(refresh),
        check: Box::new(check),
    }
}

/// A page shown and freshly loaded (not only the saved copy).
fn loaded(app: &App, target: &Target, min_shelves: usize) -> bool {
    app.page_state(target).is_some_and(|s| {
        !s.cached
            && !s.loading
            && s.page
                .as_ref()
                .is_some_and(|p| p.shelves.len() >= min_shelves)
    })
}

fn current_loaded(app: &App) -> bool {
    app.page_state(&app.view.target())
        .is_some_and(|s| !s.cached && !s.loading && s.page.is_some())
}

/// This process's resident memory in MB (mpv runs as its own process).
fn rss_mb(_: &App) -> Value {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb: Option<u64> = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches(" kB").trim().parse().ok());
    json!(kb.map(|kb| kb / 1024))
}

fn current_id(app: &App) -> Value {
    json!(
        app.playback
            .index
            .and_then(|i| app.queue.get(i))
            .map(|t| t.video_id.clone())
    )
}

fn playing_track(app: &App) -> Value {
    let track = app.playback.index.and_then(|i| app.queue.get(i));
    json!({
        "video_id": track.map(|t| t.video_id.clone()),
        "format": app.playback.format,
        "position": app.playback.position,
        "duration": app.playback.duration,
        "playing": app.playback.playing,
        "next_ready": app.playback.next_ready,
    })
}

/// A playlist from the account's library with enough songs to exercise the queue.
fn library_playlist(app: &App) -> Option<String> {
    let page = app
        .page_state(&LibraryTab::Playlists.target())?
        .page
        .as_ref()?;
    page.shelves.iter().flat_map(|s| &s.items).find_map(|i| {
        let count: u32 = i
            .subtitle
            .iter()
            .map(|r| r.text.as_str())
            .collect::<String>()
            .split(" • ")
            .find_map(|part| {
                part.strip_suffix(" tracks")
                    .or_else(|| part.strip_suffix(" songs"))
                    .and_then(|n| n.replace(',', "").parse().ok())
            })?;
        (count >= 8 && !i.title.starts_with("Liked")).then(|| i.title.clone())
    })
}

fn first_item_title(
    app: &App,
    target: &Target,
    pick: impl Fn(&crate::model::Item) -> bool,
) -> Option<String> {
    let page = app.page_state(target)?.page.as_ref()?;
    page.shelves
        .iter()
        .flat_map(|s| &s.items)
        .find(|i| pick(i))
        .map(|i| i.title.clone())
}

// ---- account: likes, library, subscriptions, playlists ----

/// Daft Punk's "Discovery" and Daft Punk: neither in the account's library
/// nor subscriptions on 2026-10-01. The run checks that before changing
/// anything and puts everything back.
const E2E_ALBUM: &str = "MPREb_7ltM34kr0mH";
const E2E_ARTIST: &str = "UCRr1xG_2WIDs18a6cIiCxeA";
const E2E_PLAYLIST: &str = "ytfast E2E";
const E2E_RENAMED: &str = "ytfast E2E renamed";
const E2E_DESCRIBED: &str = "Edited by the ytfast E2E run";

/// What the account scenario learns as it goes (songs picked, the new playlist).
static FACTS: std::sync::Mutex<BTreeMap<&'static str, String>> =
    std::sync::Mutex::new(BTreeMap::new());
/// When the last change was confirmed; a page counts as refetched only after it.
static MARK: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

fn fact(key: &str) -> Option<String> {
    FACTS.lock().expect("facts lock").get(key).cloned()
}

fn set_fact(key: &'static str, value: String) {
    FACTS.lock().expect("facts lock").insert(key, value);
}

fn set_mark() {
    *MARK.lock().expect("mark lock") = Some(Instant::now());
}

/// The page as fetched from YouTube Music after the last mark.
fn refetched<'a>(app: &'a App, target: &Target) -> Option<&'a crate::model::Page> {
    let mark = (*MARK.lock().expect("mark lock"))?;
    let state = app.page_state(target)?;
    (!state.loading && !state.cached && state.fetched.is_some_and(|f| f > mark))
        .then_some(state.page.as_ref())
        .flatten()
}

fn browse_ids(page: &crate::model::Page) -> Vec<String> {
    page.shelves
        .iter()
        .flat_map(|s| &s.items)
        .filter_map(|i| match &i.target {
            Some(Target::Browse { id, .. }) => Some(id.clone()),
            _ => None,
        })
        .collect()
}

fn song_ids(page: &crate::model::Page) -> Vec<String> {
    page.shelves
        .iter()
        .flat_map(|s| &s.items)
        .filter_map(|i| i.track.as_ref().map(|t| t.video_id.clone()))
        .collect()
}

fn subscriptions() -> Target {
    Target::browse("FEmusic_library_corpus_artists")
}

fn created_playlist() -> Option<Target> {
    fact("playlist").map(|id| Target::browse(format!("VL{id}")))
}

fn facts(keys: &[&str]) -> Option<Vec<String>> {
    keys.iter().map(|k| fact(k)).collect()
}

/// The created playlist's songs on the screen now.
fn shown_rows(app: &App) -> Vec<String> {
    created_playlist()
        .and_then(|t| app.page_state(&t)?.page.as_ref().map(song_ids))
        .unwrap_or_default()
}

/// The created playlist's songs as YouTube Music lists them after the mark.
fn listed_rows(app: &App) -> Option<Vec<String>> {
    refetched(app, &created_playlist()?).map(song_ids)
}

fn refresh_playlist(app: &mut App) {
    if let Some(target) = created_playlist() {
        app.ensure_page(target, true);
    }
}

fn rated(app: &App, key: &str) -> Option<crate::model::LikeStatus> {
    app.account_state.fetched_likes.get(&fact(key)?).copied()
}

fn ask_rating(app: &mut App, key: &str) {
    if let Some(id) = fact(key) {
        app.account_state.fetched_likes.remove(&id);
        app.backend.send(Command::LikeStatus(id));
    }
}

fn shown_like(app: &App, key: &str) -> Option<crate::model::LikeStatus> {
    let id = fact(key)?;
    let track = app
        .page_state(&Target::browse(E2E_ALBUM))?
        .page
        .as_ref()?
        .shelves
        .iter()
        .flat_map(|s| &s.items)
        .filter_map(|i| i.track.as_ref())
        .find(|t| t.video_id == id)?;
    Some(app.account_state.marks.like(track))
}

fn header_of<'a>(app: &'a App, target: &Target) -> Option<&'a crate::model::Header> {
    app.page_state(target)?.page.as_ref()?.header.as_ref()
}

fn dialog_open(app: &App, kind: &str) -> bool {
    use crate::account::Dialog;
    matches!(
        (&app.account_state.dialog, kind),
        (Some(Dialog::NewPlaylist { .. }), "new")
            | (Some(Dialog::EditPlaylist { .. }), "edit")
            | (Some(Dialog::DeletePlaylist { .. }), "delete")
            | (Some(Dialog::AddToPlaylist { .. }), "add")
    )
}

/// What the run may change, read from YouTube Music: saved albums,
/// subscriptions, playlists, and the rating of the song it likes.
fn account_snapshot(app: &App) -> Value {
    let sorted = |target: Target| {
        let mut ids = app
            .page_state(&target)
            .and_then(|s| s.page.as_ref())
            .map(browse_ids)
            .unwrap_or_default();
        ids.sort();
        ids
    };
    json!({
        "albums": sorted(LibraryTab::Albums.target()),
        "subscriptions": sorted(subscriptions()),
        "playlists": sorted(LibraryTab::Playlists.target()),
        "song": fact("liked"),
        "song_rating": rated(app, "liked").map(|r| format!("{r:?}")),
    })
}

fn refresh_account(app: &mut App) {
    set_mark();
    app.ensure_page(LibraryTab::Albums.target(), true);
    app.ensure_page(subscriptions(), true);
    app.ensure_page(LibraryTab::Playlists.target(), true);
    ask_rating(app, "liked");
}

fn account_refetched(app: &App) -> bool {
    refetched(app, &LibraryTab::Albums.target()).is_some()
        && refetched(app, &subscriptions()).is_some()
        && refetched(app, &LibraryTab::Playlists.target()).is_some()
        && rated(app, "liked").is_some()
}

fn idle(app: &App) -> bool {
    !app.account_state.busy()
}

/// Likes and unlikes a song (player bar, then a row), saves and removes an
/// album, subscribes and unsubscribes, and creates, fills (picker from a
/// row, picker from the player bar, a drag onto the sidebar), reorders,
/// trims, renames and deletes a playlist. Each change is confirmed by
/// fetching the account again; the account ends as it started.
fn account() -> Vec<Step> {
    let album = Target::browse(E2E_ALBUM);
    let artist = Target::browse(E2E_ARTIST);
    vec![
        wait("signed in", 60.0, |a| {
            matches!(a.account, Account::SignedIn { .. })
        }),
        run("open Discovery", |a| {
            a.open(View::Page(Target::browse(E2E_ALBUM)))
        }),
        wait("album page", 60.0, current_loaded),
        // Three songs: the first one not rated is liked and played.
        run("pick songs", |a| {
            let Some(page) = a
                .page_state(&Target::browse(E2E_ALBUM))
                .and_then(|s| s.page.as_ref())
            else {
                return;
            };
            let tracks: Vec<&crate::model::Track> = page
                .shelves
                .iter()
                .flat_map(|s| &s.items)
                .filter_map(|i| i.track.as_ref())
                .collect();
            let Some(liked) = tracks
                .iter()
                .find(|t| t.like == Some(crate::model::LikeStatus::Indifferent))
            else {
                return;
            };
            let mut others = tracks.iter().filter(|t| t.video_id != liked.video_id);
            if let (Some(first), Some(third)) = (others.next(), others.next()) {
                set_fact("liked", liked.video_id.clone());
                set_fact("liked_title", liked.title.clone());
                set_fact("first", first.video_id.clone());
                set_fact("first_title", first.title.clone());
                set_fact("third", third.video_id.clone());
                set_fact("third_title", third.title.clone());
            }
        }),
        wait("an unrated song and two more", 1.0, |_| {
            fact("third").is_some()
        }),
        // Before: what the run may change.
        run("fetch the account", refresh_account),
        wait("account fetched", 60.0, account_refetched),
        measure("account_before", account_snapshot),
        run("remember it", |a| {
            set_fact("before", account_snapshot(a).to_string())
        }),
        wait("song not rated", 1.0, |a| {
            rated(a, "liked") == Some(crate::model::LikeStatus::Indifferent)
        }),
        wait("album not in the library", 1.0, |a| {
            refetched(a, &LibraryTab::Albums.target())
                .is_some_and(|p| !browse_ids(p).iter().any(|id| id == E2E_ALBUM))
        }),
        wait("artist not subscribed", 1.0, |a| {
            refetched(a, &subscriptions())
                .is_some_and(|p| !browse_ids(p).iter().any(|id| id == E2E_ARTIST))
        }),
        wait("no playlist left from an earlier run", 1.0, |a| {
            a.page_state(&LibraryTab::Playlists.target())
                .and_then(|s| s.page.as_ref())
                .is_some_and(|p| {
                    !p.shelves
                        .iter()
                        .flat_map(|s| &s.items)
                        .any(|i| i.title == E2E_PLAYLIST || i.title == E2E_RENAMED)
                })
        }),
        // Like from the player bar: play the song, pause before it counts as a play.
        run("play the song", |a| {
            let Some(page) = a
                .page_state(&Target::browse(E2E_ALBUM))
                .and_then(|s| s.page.as_ref())
            else {
                return;
            };
            let tracks: Vec<crate::model::Track> = page
                .shelves
                .iter()
                .flat_map(|s| &s.items)
                .filter_map(|i| i.track.clone())
                .collect();
            let start = tracks
                .iter()
                .position(|t| Some(&t.video_id) == fact("liked").as_ref())
                .unwrap_or(0);
            a.backend.send(Command::PlayTracks { tracks, start });
        }),
        wait("song playing", 90.0, |a| {
            a.playback.playing && a.playback.position > 0.3 && current_id(a) == json!(fact("liked"))
        }),
        run("pause before ten seconds", |a| {
            if a.playback.playing {
                a.backend.send(Command::TogglePause);
            }
        }),
        wait("paused", 10.0, |a| !a.playback.playing),
        measure("paused_at", |a| json!(a.playback.position)),
        Step::Screenshot("account-01-not-liked"),
        click("Like"),
        wait("liked at once", 0.5, |a| {
            shown_like(a, "liked") == Some(crate::model::LikeStatus::Like)
        }),
        Step::Screenshot("account-02-liked"),
        wait("like accepted", 30.0, idle),
        poll(
            "like confirmed by a fresh watch-next",
            30.0,
            |a| ask_rating(a, "liked"),
            |a| rated(a, "liked") == Some(crate::model::LikeStatus::Like),
        ),
        click("Open player"),
        Step::Sleep(1.5),
        Step::Screenshot("account-03-now-playing-liked"),
        click("Close player"),
        // Unlike from the song's row.
        hover_with("the liked song's row", |_| fact("liked_title")),
        Step::Sleep(0.5),
        Step::Screenshot("account-04-row-liked"),
        click_with("the row's like button", |_| {
            fact("liked_title").map(|t| format!("Like “{t}”"))
        }),
        wait("unliked at once", 0.5, |a| {
            shown_like(a, "liked") == Some(crate::model::LikeStatus::Indifferent)
        }),
        wait("unlike accepted", 30.0, idle),
        poll(
            "unlike confirmed by a fresh watch-next",
            30.0,
            |a| ask_rating(a, "liked"),
            |a| rated(a, "liked") == Some(crate::model::LikeStatus::Indifferent),
        ),
        // Save the album, see it in Library → Albums, remove it.
        Step::Screenshot("account-05-album"),
        click("Save to library"),
        wait("saved at once", 0.5, move |a| {
            header_of(a, &album)
                .and_then(|h| h.library.as_ref())
                .is_some_and(|l| a.account_state.marks.saved(l))
        }),
        Step::Screenshot("account-06-album-saved"),
        wait("save accepted", 30.0, idle),
        run("mark", |_| set_mark()),
        poll(
            "album listed in Library → Albums",
            40.0,
            |a| a.ensure_page(LibraryTab::Albums.target(), true),
            |a| {
                refetched(a, &LibraryTab::Albums.target())
                    .is_some_and(|p| browse_ids(p).iter().any(|id| id == E2E_ALBUM))
            },
        ),
        run("open Library → Albums", |a| {
            a.open(View::Library(LibraryTab::Albums))
        }),
        Step::Sleep(2.0),
        Step::Screenshot("account-07-library-albums"),
        run("open Discovery again", |a| {
            a.open(View::Page(Target::browse(E2E_ALBUM)))
        }),
        wait("album page again", 60.0, current_loaded),
        click("Remove from library"),
        wait("removal accepted", 30.0, idle),
        run("mark", |_| set_mark()),
        poll(
            "album gone from Library → Albums",
            40.0,
            |a| a.ensure_page(LibraryTab::Albums.target(), true),
            |a| {
                refetched(a, &LibraryTab::Albums.target())
                    .is_some_and(|p| !browse_ids(p).iter().any(|id| id == E2E_ALBUM))
            },
        ),
        // Subscribe to the artist, confirm, unsubscribe.
        run("open Daft Punk", |a| {
            a.open(View::Page(Target::browse(E2E_ARTIST)))
        }),
        wait("artist page", 60.0, current_loaded),
        Step::Sleep(2.0),
        Step::Screenshot("account-08-artist"),
        click("Subscribe"),
        Step::Screenshot("account-09-subscribed"),
        wait("subscription accepted", 30.0, idle),
        run("mark", |_| set_mark()),
        poll(
            "subscription confirmed",
            40.0,
            |a| {
                a.ensure_page(Target::browse(E2E_ARTIST), true);
                a.ensure_page(subscriptions(), true);
            },
            |a| {
                refetched(a, &Target::browse(E2E_ARTIST))
                    .and_then(|p| p.header.as_ref()?.subscription.as_ref())
                    .is_some_and(|s| s.subscribed)
                    && refetched(a, &subscriptions())
                        .is_some_and(|p| browse_ids(p).iter().any(|id| id == E2E_ARTIST))
            },
        ),
        click("Subscribed"),
        wait("unsubscribe accepted", 30.0, idle),
        run("mark", |_| set_mark()),
        poll(
            "unsubscribed",
            40.0,
            |a| {
                a.ensure_page(Target::browse(E2E_ARTIST), true);
                a.ensure_page(subscriptions(), true);
            },
            move |a| {
                refetched(a, &artist)
                    .and_then(|p| p.header.as_ref()?.subscription.as_ref())
                    .is_some_and(|s| !s.subscribed)
                    && refetched(a, &subscriptions())
                        .is_some_and(|p| !browse_ids(p).iter().any(|id| id == E2E_ARTIST))
            },
        ),
        // A new playlist.
        click("Library"),
        click("Playlists"),
        wait("library playlists", 60.0, |a| {
            a.view == View::Library(LibraryTab::Playlists) && current_loaded(a)
        }),
        click("New playlist"),
        wait("new playlist dialog", 5.0, |a| dialog_open(a, "new")),
        Step::Sleep(0.5),
        Step::Type(E2E_PLAYLIST.into()),
        click("Description"),
        Step::Type("Made by the ytfast E2E run".into()),
        Step::Screenshot("account-10-new-playlist"),
        click("Create"),
        wait("playlist created", 30.0, |a| {
            idle(a)
                && a.account_state
                    .created
                    .as_ref()
                    .is_some_and(|(t, _)| t == E2E_PLAYLIST)
        }),
        run("remember the playlist", |a| {
            if let Some((_, id)) = &a.account_state.created {
                set_fact("playlist", id.clone());
            }
            set_mark();
        }),
        Step::Screenshot("account-11-created"),
        poll(
            "playlist listed in Library",
            40.0,
            |a| a.ensure_page(LibraryTab::Playlists.target(), true),
            |a| {
                let id = fact("playlist");
                refetched(a, &LibraryTab::Playlists.target()).is_some_and(|p| {
                    p.shelves
                        .iter()
                        .flat_map(|s| &s.items)
                        .any(|i| i.editable.is_some() && i.editable == id)
                })
            },
        ),
        // First song: a row's Add to playlist, then the picker's filter and Enter.
        run("open Discovery for songs", |a| {
            a.open(View::Page(Target::browse(E2E_ALBUM)))
        }),
        wait("album page for songs", 60.0, current_loaded),
        hover_with("the first song's row", |_| fact("first_title")),
        Step::Sleep(0.5),
        click_with("the row's Add to playlist", |_| {
            fact("first_title").map(|t| format!("Add “{t}” to a playlist"))
        }),
        wait("picker", 5.0, |a| dialog_open(a, "add")),
        Step::Sleep(0.5),
        Step::Type(E2E_PLAYLIST.into()),
        Step::Sleep(0.5),
        Step::Screenshot("account-12-picker"),
        Step::Key(egui::Key::Enter),
        wait("first song added", 30.0, |a| {
            idle(a) && a.account_state.dialog.is_none()
        }),
        // Second: the playing song, from the player bar's picker.
        click("Add to playlist"),
        wait("picker from the player bar", 5.0, |a| dialog_open(a, "add")),
        Step::Sleep(0.5),
        click(E2E_PLAYLIST),
        wait("second song added", 30.0, |a| {
            idle(a) && a.account_state.dialog.is_none()
        }),
        // Third: dragged onto the playlist in the sidebar.
        drag_with(
            "a song row onto the sidebar playlist",
            |_| fact("third_title"),
            |_| Some(E2E_PLAYLIST.to_owned()),
        ),
        Step::Sleep(1.0),
        wait("third song added", 30.0, idle),
        run("open the playlist", |a| {
            if let Some(target) = created_playlist() {
                a.open(View::Page(target));
            }
            set_mark();
        }),
        poll("three songs listed", 40.0, refresh_playlist, |a| {
            listed_rows(a) == facts(&["first", "liked", "third"])
        }),
        Step::Sleep(2.0),
        Step::Screenshot("account-13-three-songs"),
        // Reorder: the third song onto the first.
        drag_with(
            "the last song onto the first",
            |_| fact("third_title"),
            |_| fact("first_title"),
        ),
        wait("moved at once", 2.0, |a| {
            Some(shown_rows(a)) == facts(&["third", "first", "liked"])
        }),
        Step::Screenshot("account-14-moved"),
        wait("move accepted", 30.0, idle),
        run("mark", |_| set_mark()),
        poll("new order listed", 40.0, refresh_playlist, |a| {
            listed_rows(a) == facts(&["third", "first", "liked"])
        }),
        // Remove one.
        hover_with("the first song's row in the playlist", |_| {
            fact("first_title")
        }),
        Step::Sleep(0.5),
        click_with("the row's Remove", |_| {
            fact("first_title").map(|t| format!("Remove “{t}” from the playlist"))
        }),
        wait("removed at once", 2.0, |a| {
            Some(shown_rows(a)) == facts(&["third", "liked"])
        }),
        wait("removal from the playlist accepted", 30.0, idle),
        run("mark", |_| set_mark()),
        poll("removal listed", 40.0, refresh_playlist, |a| {
            listed_rows(a) == facts(&["third", "liked"])
        }),
        // Rename and describe.
        click("Edit playlist"),
        wait("edit dialog", 5.0, |a| dialog_open(a, "edit")),
        Step::Sleep(0.5),
        Step::Type(E2E_RENAMED.into()),
        click("Description"),
        Step::Type(E2E_DESCRIBED.into()),
        Step::Screenshot("account-15-edit"),
        click("Save"),
        wait("renamed at once", 2.0, |a| {
            created_playlist()
                .and_then(|t| header_of(a, &t).map(|h| h.title == E2E_RENAMED))
                .unwrap_or(false)
        }),
        wait("rename accepted", 30.0, idle),
        run("mark", |_| set_mark()),
        poll(
            "new name and description listed",
            40.0,
            refresh_playlist,
            |a| {
                created_playlist()
                    .and_then(|t| refetched(a, &t)?.header.clone())
                    .is_some_and(|h| {
                        h.title == E2E_RENAMED && h.description.as_deref() == Some(E2E_DESCRIBED)
                    })
            },
        ),
        Step::Sleep(1.0),
        Step::Screenshot("account-16-renamed"),
        // Delete.
        click("Delete playlist"),
        wait("delete dialog", 5.0, |a| dialog_open(a, "delete")),
        Step::Screenshot("account-17-delete"),
        click("Delete"),
        wait("left the deleted playlist", 2.0, |a| {
            created_playlist().is_none_or(|t| a.view != View::Page(t))
        }),
        wait("delete accepted", 30.0, idle),
        run("mark", |_| set_mark()),
        poll(
            "playlist gone from Library",
            40.0,
            |a| a.ensure_page(LibraryTab::Playlists.target(), true),
            |a| {
                let id = fact("playlist");
                refetched(a, &LibraryTab::Playlists.target()).is_some_and(|p| {
                    !p.shelves
                        .iter()
                        .flat_map(|s| &s.items)
                        .any(|i| i.editable.is_some() && i.editable == id)
                })
            },
        ),
        Step::Screenshot("account-18-deleted"),
        // After: the account as it was.
        run("fetch the account again", refresh_account),
        wait("account fetched again", 60.0, account_refetched),
        measure("account_after", account_snapshot),
        wait("account as found", 1.0, |a| {
            fact("before") == Some(account_snapshot(a).to_string())
        }),
    ]
}

fn journey() -> Vec<Step> {
    let home = View::Home.target();
    vec![
        wait("signed in", 60.0, |a| {
            matches!(a.account, Account::SignedIn { .. })
        }),
        wait("home loaded", 60.0, move |a| loaded(a, &home, 2)),
        measure("first_frame_ms", |a| {
            json!(a.first_frame.map(|d| d.as_millis()))
        }),
        Step::Sleep(4.0),
        Step::Screenshot("01-home"),
        measure("rss_mb_home", rss_mb),
        // Playback through a playlist from the library.
        wait("library playlists", 60.0, |a| library_playlist(a).is_some()),
        click_with("a library playlist with 8+ songs", library_playlist),
        wait("playlist page", 60.0, current_loaded),
        Step::Sleep(3.0),
        Step::Screenshot("02-playlist"),
        click("Play"),
        wait("playing", 60.0, |a| {
            a.playback.playing && a.playback.position > 0.5
        }),
        measure("first_track", playing_track),
        Step::Screenshot("03-playing"),
        wait("next track prepared", 120.0, |a| a.playback.next_ready),
        measure("prepared", playing_track),
        // A gapless change: seek near the end and watch the next track take over.
        run("seek near the end", |a| {
            let end = (a.playback.duration - 4.0).max(0.0);
            a.backend.send(Command::Seek(end));
        }),
        measure("before_auto_change", current_id),
        wait("automatic change", 30.0, |a| {
            a.playback.index == Some(1) && a.playback.playing
        }),
        measure("after_auto_change", playing_track),
        // Seek by clicking the middle of the progress bar.
        Step::Sleep(2.0),
        click("Seek"),
        wait("seeked to the middle", 15.0, |a| {
            a.playback.duration > 0.0 && a.playback.position > a.playback.duration * 0.3
        }),
        measure("after_seek", playing_track),
        click("Next"),
        wait("third track", 60.0, |a| {
            a.playback.index == Some(2) && a.playback.playing && a.playback.position > 0.3
        }),
        measure("third_track", playing_track),
        wait("next prepared again", 120.0, |a| a.playback.next_ready),
        click("Next"),
        wait("fourth track", 60.0, |a| {
            a.playback.index == Some(3) && a.playback.playing && a.playback.position > 0.3
        }),
        measure("fourth_track", playing_track),
        // Now Playing.
        click("Open player"),
        wait("now playing", 10.0, |a| a.now_playing),
        Step::Sleep(2.0),
        Step::Screenshot("04-up-next"),
        click("LYRICS"),
        Step::Sleep(4.0),
        Step::Screenshot("05-lyrics"),
        measure("lyrics", |a| {
            json!(
                a.playback
                    .lyrics
                    .as_ref()
                    .map(|id| match a.lyrics.get(id) {
                        Some(Ok(Some(_))) => "shown",
                        Some(Ok(None)) => "none",
                        Some(Err(_)) => "error",
                        None => "loading",
                    })
                    .unwrap_or("not offered")
            )
        }),
        click("RELATED"),
        wait("related", 30.0, |a| {
            a.playback
                .related
                .as_ref()
                .is_some_and(|id| loaded(a, &Target::browse(id.clone()), 1))
        }),
        Step::Sleep(3.0),
        Step::Screenshot("06-related"),
        click("Close player"),
        // Explore and Library.
        click("Explore"),
        wait("explore", 60.0, |a| loaded(a, &View::Explore.target(), 2)),
        Step::Sleep(3.0),
        Step::Screenshot("07-explore"),
        click("Library"),
        wait("library playlists", 60.0, current_loaded),
        Step::Sleep(2.0),
        Step::Screenshot("08-library-playlists"),
        click("Songs"),
        wait("library songs", 60.0, current_loaded),
        Step::Sleep(2.0),
        Step::Screenshot("09-library-songs"),
        click("Albums"),
        wait("library albums", 60.0, current_loaded),
        Step::Sleep(1.0),
        Step::Screenshot("10-library-albums"),
        click("Artists"),
        wait("library artists", 60.0, current_loaded),
        Step::Sleep(2.0),
        Step::Screenshot("11-library-artists"),
        click_with("the first library artist", |a| {
            first_item_title(a, &LibraryTab::Artists.target(), |_| true)
        }),
        wait("artist page", 60.0, |a| {
            current_loaded(a) && matches!(a.view, View::Page(_))
        }),
        Step::Sleep(3.0),
        Step::Screenshot("12-artist"),
        // An album from Home's new releases.
        click("Home"),
        wait("home again", 30.0, |a| a.view == View::Home),
        click_with("an album on Home", |a| {
            first_item_title(a, &View::Home.target(), |i| {
                i.kind == crate::model::ItemKind::Album
            })
        }),
        wait("album page", 60.0, |a| {
            current_loaded(a) && matches!(a.view, View::Page(_))
        }),
        Step::Sleep(3.0),
        Step::Screenshot("13-album"),
        // Search.
        click("Search"),
        Step::Type("liquid drum and bass".into()),
        Step::Sleep(2.0),
        Step::Screenshot("14-suggestions"),
        Step::Key(egui::Key::Enter),
        wait("search results", 60.0, |a| {
            matches!(&a.view, View::Page(Target::Search { .. })) && current_loaded(a)
        }),
        Step::Sleep(3.0),
        Step::Screenshot("15-search"),
        click("Settings"),
        Step::Sleep(1.0),
        Step::Screenshot("16-settings"),
        Step::Key(egui::Key::Escape),
        // History: the plays above should be at the top of the account's history.
        Step::Sleep(5.0),
        run("load history", |a| {
            a.ensure_page(Target::browse("FEmusic_history"), true)
        }),
        wait("history", 60.0, |a| {
            loaded(a, &Target::browse("FEmusic_history"), 1)
        }),
        measure("history_top", |a| {
            let page = a
                .page_state(&Target::browse("FEmusic_history"))
                .and_then(|s| s.page.as_ref());
            json!(page.map(|p| {
                p.shelves
                    .iter()
                    .flat_map(|s| &s.items)
                    .filter_map(|i| i.track.as_ref())
                    .take(6)
                    .map(|t| t.video_id.clone())
                    .collect::<Vec<_>>()
            }))
        }),
        measure("played", |a| {
            json!(
                a.queue
                    .iter()
                    .take(4)
                    .map(|t| t.video_id.clone())
                    .collect::<Vec<_>>()
            )
        }),
        measure("rss_mb_end", rss_mb),
        run("pause", |a| {
            if a.playback.playing {
                a.backend.send(Command::TogglePause);
            }
        }),
    ]
}

enum Phase {
    Idle,
    Move(Pos2),
    Press(Pos2),
    Release,
}

/// The visible area of the control named `wanted` (the last match: page
/// content is drawn after the chrome, dialogs last).
fn find_control(
    registry: &[(String, Rect)],
    ctx: &egui::Context,
    wanted: Option<&str>,
) -> Option<Rect> {
    let wanted = wanted?;
    let screen = ctx.content_rect();
    registry
        .iter()
        .rev()
        .find(|(l, r)| l == wanted && screen.contains(r.center()))
        .map(|(_, r)| *r)
}

pub struct Driver {
    dir: PathBuf,
    steps: Vec<Step>,
    index: usize,
    step_started: Instant,
    started: Instant,
    log: std::fs::File,
    pending: Vec<Event>,
    phase: Phase,
    screenshot_requested: bool,
    measurements: BTreeMap<String, Value>,
    failures: Vec<String>,
    finished: bool,
    /// A drag in progress: start, end and frame.
    drag: Option<(Pos2, Pos2, u32)>,
    /// When a polling step last refreshed.
    polled: Option<Instant>,
}

impl Driver {
    pub fn from_env() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("YTFAST_E2E_DIR")?);
        std::fs::create_dir_all(&dir).ok()?;
        let log = std::fs::File::create(dir.join("log.txt")).ok()?;
        Some(Self {
            dir,
            steps: scenario(&std::env::var("YTFAST_E2E_SCENARIO").unwrap_or_default()),
            index: 0,
            step_started: Instant::now(),
            started: Instant::now(),
            log,
            pending: Vec::new(),
            phase: Phase::Idle,
            screenshot_requested: false,
            measurements: BTreeMap::new(),
            failures: Vec::new(),
            finished: false,
            drag: None,
            polled: None,
        })
    }

    fn note(&mut self, line: &str) {
        let _ = writeln!(
            self.log,
            "[{:7.2}s] {line}",
            self.started.elapsed().as_secs_f64()
        );
        log::info!("e2e: {line}");
    }

    fn advance(&mut self) {
        self.index += 1;
        self.step_started = Instant::now();
        self.phase = Phase::Idle;
        self.screenshot_requested = false;
        self.drag = None;
        self.polled = None;
    }

    fn fail(&mut self, message: String) {
        self.note(&format!("FAIL {message}"));
        self.failures.push(message);
        self.advance();
    }

    /// Feeds the synthetic input for this frame.
    pub fn inject(&mut self, raw: &mut egui::RawInput) {
        raw.events.append(&mut self.pending);
    }

    pub fn frame(&mut self, app: &mut App, ctx: &egui::Context, registry: Vec<(String, Rect)>) {
        if self.finished {
            return;
        }
        ctx.request_repaint();
        if self.index >= self.steps.len() {
            self.finish(ctx);
            return;
        }
        // The steps are taken out while one runs, so it can log and advance.
        let steps = std::mem::take(&mut self.steps);
        self.step(&steps[self.index], app, ctx, &registry);
        self.steps = steps;
    }

    fn step(
        &mut self,
        step: &Step,
        app: &mut App,
        ctx: &egui::Context,
        registry: &[(String, Rect)],
    ) {
        let elapsed = self.step_started.elapsed().as_secs_f64();
        match step {
            Step::Wait {
                what,
                timeout,
                check,
            } => {
                if check(app) {
                    let line = format!("ok   {what} ({elapsed:.1}s)");
                    self.measurements
                        .insert(format!("wait:{what}"), json!(elapsed));
                    self.note(&line);
                    self.advance();
                } else if elapsed > *timeout {
                    self.fail(format!("timed out waiting for {what} after {timeout}s"));
                }
            }
            Step::Click {
                label,
                describe,
                timeout,
            } => match self.phase {
                Phase::Idle => {
                    let wanted = label(app);
                    let screen = ctx.content_rect();
                    // The last match: page content is drawn after the chrome.
                    let found = wanted.as_ref().and_then(|w| {
                        registry
                            .iter()
                            .rev()
                            .find(|(l, r)| l == w && screen.contains(r.center()))
                            .map(|(_, r)| *r)
                    });
                    match found {
                        Some(rect) => {
                            let line =
                                format!("click {describe} = {:?}", wanted.unwrap_or_default());
                            self.note(&line);
                            self.pending.push(Event::PointerMoved(rect.center()));
                            self.phase = Phase::Move(rect.center());
                        }
                        None if elapsed > *timeout => {
                            let message =
                                format!("no visible control named {describe} ({wanted:?})");
                            self.fail(message);
                        }
                        None => {}
                    }
                }
                Phase::Move(pos) => {
                    self.pending.push(Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    });
                    self.phase = Phase::Press(pos);
                }
                Phase::Press(pos) => {
                    self.pending.push(Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default(),
                    });
                    self.phase = Phase::Release;
                }
                Phase::Release => {
                    self.pending.push(Event::PointerGone);
                    self.advance();
                }
            },
            Step::Type(text) => {
                self.pending.push(Event::Text(text.clone()));
                let line = format!("type {text:?}");
                self.note(&line);
                self.advance();
            }
            Step::Key(key) => {
                let key = *key;
                for pressed in [true, false] {
                    self.pending.push(Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: Default::default(),
                    });
                }
                self.note(&format!("key {key:?}"));
                self.advance();
            }
            Step::Screenshot(name) => {
                let name = *name;
                if !self.screenshot_requested {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                    self.screenshot_requested = true;
                } else if let Some(image) = ctx.input(|i| {
                    i.raw.events.iter().find_map(|e| match e {
                        Event::Screenshot { image, .. } => Some(image.clone()),
                        _ => None,
                    })
                }) {
                    let path = self.dir.join(format!("{name}.png"));
                    let pixels: Vec<u8> = image
                        .pixels
                        .iter()
                        .flat_map(|c| c.to_srgba_unmultiplied())
                        .collect();
                    match image::save_buffer(
                        &path,
                        &pixels,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    ) {
                        Ok(()) => self.note(&format!("screenshot {}", path.display())),
                        Err(error) => self.failures.push(format!("screenshot {name}: {error}")),
                    }
                    self.advance();
                } else if elapsed > 10.0 {
                    self.fail(format!("no screenshot for {name}"));
                }
            }
            Step::Measure { name, value } => {
                let name = *name;
                let value = value(app);
                self.note(&format!("measure {name} = {value}"));
                self.measurements.insert(name.to_owned(), value);
                self.advance();
            }
            Step::Sleep(seconds) => {
                if elapsed >= *seconds {
                    self.advance();
                }
            }
            Step::Run(what, f) => {
                let what = *what;
                f(app);
                self.note(&format!("run {what}"));
                self.advance();
            }
            Step::Hover { label, describe } => {
                let wanted = label(app);
                match find_control(registry, ctx, wanted.as_deref()) {
                    Some(rect) => {
                        self.note(&format!("hover {describe} = {wanted:?}"));
                        self.pending.push(Event::PointerMoved(rect.center()));
                        self.advance();
                    }
                    None if elapsed > 15.0 => {
                        self.fail(format!("no visible control named {describe} ({wanted:?})"));
                    }
                    None => {}
                }
            }
            Step::Drag { from, to, describe } => match self.drag {
                None => {
                    let (a, b) = (from(app), to(app));
                    let found = find_control(registry, ctx, a.as_deref()).zip(find_control(
                        registry,
                        ctx,
                        b.as_deref(),
                    ));
                    match found {
                        Some((start, end)) => {
                            self.note(&format!("drag {describe}: {a:?} onto {b:?}"));
                            self.pending.push(Event::PointerMoved(start.center()));
                            self.drag = Some((start.center(), end.center(), 0));
                        }
                        None if elapsed > 20.0 => {
                            self.fail(format!(
                                "no visible controls for drag {describe} ({a:?}, {b:?})"
                            ));
                        }
                        None => {}
                    }
                }
                // Press, move in ten frames, rest on the target, release.
                Some((start, end, frame)) => {
                    const MOVES: u32 = 10;
                    let button = |pos, pressed| Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    };
                    match frame {
                        0 => self.pending.push(button(start, true)),
                        f if f <= MOVES => {
                            let t = f as f32 / MOVES as f32;
                            self.pending.push(Event::PointerMoved(start.lerp(end, t)));
                        }
                        f if f == MOVES + 1 => self.pending.push(Event::PointerMoved(end)),
                        f if f == MOVES + 2 => self.pending.push(button(end, false)),
                        _ => {
                            self.pending.push(Event::PointerGone);
                            self.advance();
                            return;
                        }
                    }
                    self.drag = Some((start, end, frame + 1));
                }
            },
            Step::Poll {
                what,
                timeout,
                every,
                refresh,
                check,
            } => {
                if check(app) {
                    self.note(&format!("ok   {what} ({elapsed:.1}s)"));
                    self.measurements
                        .insert(format!("wait:{what}"), json!(elapsed));
                    self.advance();
                } else if elapsed > *timeout {
                    self.fail(format!("timed out waiting for {what} after {timeout}s"));
                } else if self
                    .polled
                    .is_none_or(|t| t.elapsed().as_secs_f64() >= *every)
                {
                    refresh(app);
                    self.polled = Some(Instant::now());
                }
            }
        }
    }

    fn finish(&mut self, ctx: &egui::Context) {
        self.finished = true;
        let summary = json!({
            "passed": self.failures.is_empty(),
            "failures": self.failures,
            "steps": self.steps.len(),
            "seconds": self.started.elapsed().as_secs_f64(),
            "measurements": self.measurements,
            "candidate": option_env!("YTFAST_COMMIT").unwrap_or("unknown"),
        });
        let _ = std::fs::write(
            self.dir.join("summary.json"),
            serde_json::to_vec_pretty(&summary).unwrap_or_default(),
        );
        let line = format!("done: {} failure(s)", self.failures.len());
        self.note(&line);
        // Give the last log line a moment, then close.
        std::thread::sleep(Duration::from_millis(200));
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}
