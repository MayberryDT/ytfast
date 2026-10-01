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

/// What the backend read back from mpv (gains, `af`, the sleep fade), for
/// the engine scenarios.
static PROBES: std::sync::Mutex<BTreeMap<String, Value>> = std::sync::Mutex::new(BTreeMap::new());

pub fn probe(key: &str, value: Value) {
    PROBES
        .lock()
        .expect("probes lock")
        .insert(key.to_owned(), value);
}

/// Adds `value` to the list under `key`.
pub fn probe_push(key: &str, value: Value) {
    let mut probes = PROBES.lock().expect("probes lock");
    match probes.entry(key.to_owned()).or_insert_with(|| json!([])) {
        Value::Array(list) => list.push(value),
        other => *other = json!([value]),
    }
}

fn probed(key: &str) -> Value {
    PROBES
        .lock()
        .expect("probes lock")
        .get(key)
        .cloned()
        .unwrap_or(Value::Null)
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
        "engine" => engine(),
        "engine-restore" => engine_restore(),
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

// ---- engine: playback control ----

/// Moments the engine scenarios time from.
static MARKS: std::sync::Mutex<BTreeMap<&'static str, Instant>> =
    std::sync::Mutex::new(BTreeMap::new());

fn mark(name: &'static str) -> Step {
    run(name, move |_| {
        MARKS
            .lock()
            .expect("marks lock")
            .insert(name, Instant::now());
    })
}

fn ms_since(name: &'static str) -> impl Fn(&App) -> Value {
    move |_| {
        json!(
            MARKS
                .lock()
                .expect("marks lock")
                .get(name)
                .map(|t| t.elapsed().as_millis() as u64)
        )
    }
}

/// A text field of something the scenario noted with [`probe`].
fn noted(key: &str, field: &str) -> Option<String> {
    probed(key)
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The songs on the page shown, as rows or cards.
fn page_items(app: &App) -> Vec<&crate::model::Item> {
    app.page_state(&app.view.target())
        .and_then(|s| s.page.as_ref())
        .map(|p| {
            p.shelves
                .iter()
                .flat_map(|s| &s.items)
                .filter(|i| i.track.is_some())
                .collect()
        })
        .unwrap_or_default()
}

fn item_id(item: &crate::model::Item) -> String {
    item.track
        .as_ref()
        .map(|t| t.video_id.clone())
        .unwrap_or_default()
}

/// Notes a song on the page (its id and the title its row is named by).
fn note_item(key: &str, item: &crate::model::Item) {
    probe(key, json!({"id": item_id(item), "title": item.title}));
}

/// Audio is coming out: mpv is past the start of the song.
fn audible(app: &App) -> bool {
    app.playback.playing && !app.playback.loading && app.playback.position > 0.05
}

fn queue_ids(app: &App) -> Vec<String> {
    app.queue.iter().map(|t| t.video_id.clone()).collect()
}

/// The next `n` songs Up next shows.
fn upcoming(app: &App, n: usize) -> Vec<String> {
    let from = app.playback.index.map_or(0, |i| i + 1);
    queue_ids(app).into_iter().skip(from).take(n).collect()
}

/// The songs the engine scenario adds with Play next and Add to queue.
fn extras() -> Vec<crate::model::Track> {
    serde_json::from_value(probed("engine:extras")).unwrap_or_default()
}

fn extra_id(i: usize) -> Option<String> {
    extras().get(i).map(|t| t.video_id.clone())
}

fn extra_title(i: usize) -> Option<String> {
    extras().get(i).map(|t| t.title.clone())
}

fn extra_ids(order: &[usize]) -> Vec<String> {
    order.iter().filter_map(|&i| extra_id(i)).collect()
}

/// What `engine` wrote for `engine-restore`, in ytfast's cache directory.
fn expected_file() -> Option<PathBuf> {
    Some(
        crate::paths::Paths::new()
            .ok()?
            .cache
            .join("e2e-engine-expected.json"),
    )
}

fn expected() -> Value {
    expected_file()
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

/// session.json as the backend last wrote it.
fn saved_session() -> Value {
    crate::paths::Paths::new()
        .ok()
        .and_then(|p| std::fs::read(p.cache.join("session.json")).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

/// The state a relaunch should bring back.
fn session_state(app: &App) -> Value {
    json!({
        "queue": queue_ids(app),
        "index": app.playback.index,
        "position": app.playback.position,
        "volume": app.playback.volume,
        "shuffle": app.playback.shuffle,
        "repeat": app.playback.repeat,
        "autoplay": app.playback.autoplay,
        "equalizer": app.playback.equalizer,
        "normalize": app.playback.normalize,
    })
}

/// session.json holds the queue, song, position and volume shown now.
fn session_saved(app: &App) -> bool {
    let saved = saved_session();
    let ids: Option<Vec<String>> = saved["queue"]["entries"].as_array().map(|entries| {
        entries
            .iter()
            .filter_map(|e| e["track"]["video_id"].as_str().map(str::to_owned))
            .collect()
    });
    ids == Some(queue_ids(app))
        && saved["index"].as_u64().map(|i| i as usize) == app.playback.index
        && saved["position"]
            .as_f64()
            .is_some_and(|p| (p - app.playback.position).abs() < 1.0)
        && saved["volume"].as_f64() == Some(app.playback.volume)
}

/// The relaunched app shows what `engine` left.
fn restored_as_expected(app: &App) -> bool {
    let want = expected();
    let now = session_state(app);
    [
        "queue",
        "index",
        "volume",
        "shuffle",
        "repeat",
        "autoplay",
        "equalizer",
    ]
    .iter()
    .all(|k| want[k] == now[k])
        && want["position"]
            .as_f64()
            .is_some_and(|p| (p - app.playback.position).abs() < 1.0)
}

/// Lets the queue play on: notes each song as it plays and seeks near its
/// end so the next one follows (gapless, as queued in mpv).
fn advance_through(app: &mut App) {
    let mut played: Vec<String> =
        serde_json::from_value(probed("engine:played")).unwrap_or_default();
    if let Value::String(id) = current_id(app)
        && played.last() != Some(&id)
    {
        played.push(id);
        probe("engine:played", json!(played));
    }
    let pb = &app.playback;
    if audible(app) && pb.duration > 10.0 && pb.position < pb.duration - 6.0 {
        app.backend.send(Command::Seek(pb.duration - 4.0));
    }
}

fn played_after_edits() -> Vec<String> {
    let played: Vec<String> = serde_json::from_value(probed("engine:played")).unwrap_or_default();
    played.into_iter().skip(1).take(4).collect()
}

fn shown_after_edits() -> Vec<String> {
    serde_json::from_value(probed("engine:shown")).unwrap_or_default()
}

/// mpv's `af` holds the Rock preset's graph (or nothing when bypassed).
fn af_is(enabled: bool) -> bool {
    let af = probed("af");
    let graph = af["mpv_af"]
        .as_array()
        .and_then(|filters| {
            filters
                .iter()
                .find(|f| f["label"] == crate::equalizer::LABEL)
        })
        .and_then(|f| f["params"]["graph"].as_str())
        .map(str::to_owned);
    af["preset"] == "Rock"
        && af["enabled"] == enabled
        && if enabled {
            graph.is_some_and(|g| g.contains("equalizer@b0=f=31:t=o:w=1:g=4.5"))
        } else {
            af["mpv_af"].as_array().is_some_and(Vec::is_empty)
        }
}

/// The last gain probe: what levelling set and what mpv has.
fn last_gain() -> Value {
    probed("gains")
        .as_array()
        .and_then(|l| l.last().cloned())
        .unwrap_or(Value::Null)
}

fn sleep_choice(app: &App) -> Option<crate::model::Sleep> {
    app.playback.sleep.map(|s| s.choice)
}

/// Control (docs/SPEC.md § Control) through the real app: how soon a cold
/// click starts an unprepared song and a prepared one; Play next, Add to
/// queue, a reorder and a remove in Up next, then the songs playing in the
/// order shown; an equalizer preset, its bypass and loudness levelling as
/// mpv has them; the sleep timer at a song's end and after a minute, with
/// its fade. It ends paused at a known place with a known volume and writes
/// what `engine-restore` (the next launch) must find.
fn engine() -> Vec<Step> {
    use crate::model::Sleep;
    vec![
        wait("signed in", 60.0, |a| {
            matches!(a.account, Account::SignedIn { .. })
        }),
        run(
            "note what to put back, start unshuffled with levelling on",
            |a| {
                probe("engine:original", session_state(a));
                if a.playback.shuffle {
                    a.backend.send(Command::ToggleShuffle);
                }
                let cycles = match a.playback.repeat {
                    crate::model::Repeat::Off => 0,
                    crate::model::Repeat::All => 2,
                    crate::model::Repeat::One => 1,
                };
                for _ in 0..cycles {
                    a.backend.send(Command::CycleRepeat);
                }
                if !a.playback.normalize {
                    a.backend.send(Command::Normalize(true));
                }
            },
        ),
        wait("library playlists", 60.0, |a| library_playlist(a).is_some()),
        click_with("a library playlist with 8+ songs", library_playlist),
        mark("page"),
        wait("playlist page", 60.0, current_loaded),
        // The first songs on screen are resolved ahead as the page shows.
        wait("the first songs on screen prepared", 120.0, |a| {
            let items = page_items(a);
            items.len() >= 8
                && items
                    .iter()
                    .take(2)
                    .all(|i| a.backend.prepared(&item_id(i)))
        }),
        measure("first_two_prepared_ms", ms_since("page")),
        measure("prepared_on_screen", |a| {
            json!(
                page_items(a)
                    .iter()
                    .take(8)
                    .map(|i| a.backend.prepared(&item_id(i)))
                    .collect::<Vec<_>>()
            )
        }),
        Step::Screenshot("e01-playlist"),
        // A cold click on a song nothing prepared, past the first rows.
        run("pick an unprepared song", |a| {
            if let Some(item) = page_items(a)
                .into_iter()
                .skip(4)
                .find(|i| !a.backend.prepared(&item_id(i)))
            {
                note_item("engine:cold", item);
            }
        }),
        mark("cold"),
        click_with("the unprepared song", |_| noted("engine:cold", "title")),
        wait("the unprepared song plays", 90.0, |a| {
            audible(a) && current_id(a) == json!(noted("engine:cold", "id"))
        }),
        measure("cold_click_unprepared_ms", ms_since("cold")),
        measure("unprepared_song", playing_track),
        // A cold click on a song prepared since the page showed.
        run("pick a prepared song", |a| {
            if let Some(item) = page_items(a)
                .into_iter()
                .take(4)
                .find(|i| a.backend.prepared(&item_id(i)) && current_id(a) != json!(item_id(i)))
            {
                note_item("engine:warm", item);
            }
        }),
        mark("warm"),
        click_with("the prepared song", |_| noted("engine:warm", "title")),
        wait("the prepared song plays", 60.0, |a| {
            audible(a) && current_id(a) == json!(noted("engine:warm", "id"))
        }),
        measure("cold_click_prepared_ms", ms_since("warm")),
        measure("prepared_song", playing_track),
        // Queue edits: Play next and Add to queue (the menus that offer them
        // send these commands), then a reorder and a remove in Up next.
        run("pick four songs from Home", |a| {
            let queued: std::collections::HashSet<String> = queue_ids(a).into_iter().collect();
            let mut extras: Vec<crate::model::Track> = Vec::new();
            for track in a
                .page_state(&View::Home.target())
                .and_then(|s| s.page.as_ref())
                .map(|p| {
                    p.shelves
                        .iter()
                        .flat_map(|s| &s.items)
                        .filter_map(|i| i.track.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
            {
                let fresh = !queued.contains(&track.video_id)
                    && !extras.iter().any(|t| t.video_id == track.video_id)
                    && !extras.iter().any(|t| t.title == track.title);
                if fresh && extras.len() < 4 {
                    extras.push(track);
                }
            }
            probe("engine:extras", json!(extras));
        }),
        wait("four songs to add", 1.0, |_| extras().len() == 4),
        run("Play next, Add to queue, Add to queue, Play next", |a| {
            if let [e0, e1, e2, e3] = extras().as_slice() {
                a.backend.send(Command::PlayNext(vec![e0.clone()]));
                a.backend.send(Command::AddToQueue(vec![e1.clone()]));
                a.backend.send(Command::AddToQueue(vec![e2.clone()]));
                a.backend.send(Command::PlayNext(vec![e3.clone()]));
            }
        }),
        wait(
            "Up next: the last Play next, then the others in order",
            10.0,
            |a| upcoming(a, 4) == extra_ids(&[3, 0, 1, 2]),
        ),
        measure("up_next_after_additions", |a| json!(upcoming(a, 6))),
        click("Open player"),
        wait("now playing", 10.0, |a| a.now_playing),
        hover_with("an added song in Up next", |_| extra_title(0)),
        Step::Sleep(1.0),
        Step::Screenshot("e02-up-next-additions"),
        drag_with(
            "the last Add to queue onto the first song after the current one",
            |_| extra_title(2).map(|t| format!("Reorder {t}")),
            |_| extra_title(3),
        ),
        wait("reordered", 10.0, |a| {
            upcoming(a, 4) == extra_ids(&[3, 2, 0, 1])
        }),
        click_with("remove an added song", |_| {
            extra_title(0).map(|t| format!("Remove {t}"))
        }),
        wait("removed", 10.0, |a| upcoming(a, 3) == extra_ids(&[3, 2, 1])),
        measure("shown_order", |a| {
            let shown = upcoming(a, 4);
            probe("engine:shown", json!(shown));
            json!(shown)
        }),
        Step::Screenshot("e03-up-next-edited"),
        poll("the next four songs played", 300.0, advance_through, |_| {
            played_after_edits().len() >= 4
        }),
        measure("played_order", |_| json!(played_after_edits())),
        wait("played in the order shown", 1.0, |_| {
            played_after_edits() == shown_after_edits()
        }),
        click("Close player"),
        // The equalizer: a preset, bypassed, back on, as mpv's `af` has it.
        click("Settings"),
        click("Open equalizer"),
        wait("equalizer open", 5.0, |a| a.equalizer_open),
        click("Rock"),
        wait("Rock in mpv", 15.0, |_| af_is(true)),
        measure("af_rock", |_| probed("af")),
        Step::Sleep(0.5),
        Step::Screenshot("e04-equalizer-rock"),
        click("Equalizer"),
        wait("bypassed in mpv", 15.0, |_| af_is(false)),
        measure("af_bypassed", |_| probed("af")),
        click("Equalizer"),
        wait("Rock back in mpv", 15.0, |_| af_is(true)),
        click("Close equalizer"),
        wait("equalizer closed", 5.0, |a| !a.equalizer_open),
        // Loudness levelling off and on, as mpv's volume-gain has it.
        measure("gains_so_far", |_| probed("gains")),
        click("Settings"),
        click("Even out loudness between songs"),
        wait("levelling off in mpv", 15.0, |a| {
            !a.playback.normalize
                && last_gain()["normalize"] == false
                && last_gain()["mpv_volume_gain"].as_f64() == Some(0.0)
        }),
        measure("gain_off", |_| last_gain()),
        click("Settings"),
        click("Even out loudness between songs"),
        wait("levelling on in mpv", 15.0, |a| {
            a.playback.normalize
                && last_gain()["normalize"] == true
                && last_gain()["mpv_volume_gain"].as_f64() == a.playback.gain
        }),
        measure("gain_on", |_| last_gain()),
        // The sleep timer: at the end of a song, then after a minute. Now
        // Playing is open, so "Play" is the player bar's.
        click("Open player"),
        wait("now playing again", 10.0, |a| a.now_playing),
        click("Sleep timer"),
        click("End of song"),
        wait("timer at the song's end", 5.0, |a| {
            sleep_choice(a) == Some(Sleep::EndOfSong)
        }),
        Step::Sleep(0.5),
        Step::Screenshot("e05-sleep-end-of-song"),
        run("note the song, seek ten seconds before its end", |a| {
            probe("engine:sleep_index", json!(a.playback.index));
            probe("sleep_fade", json!([]));
            let end = (a.playback.duration - 10.0).max(0.0);
            a.backend.send(Command::Seek(end));
        }),
        wait("stopped after the song, the next one ready", 60.0, |a| {
            let before = probed("engine:sleep_index").as_u64().map(|i| i as usize);
            !a.playback.playing
                && a.playback.sleep.is_none()
                && a.playback.index.is_some()
                && a.playback.index != before
                && a.playback.position < 1.0
        }),
        measure("end_of_song", |a| {
            json!({
                "index": a.playback.index,
                "playing": a.playback.playing,
                "fade_volumes": probed("sleep_fade"),
                "stopped": probed("sleep_stopped"),
            })
        }),
        click("Play"),
        wait("playing again", 60.0, audible),
        run("a one-minute timer", |a| {
            probe("sleep_fade", json!([]));
            probe("sleep_stopped", Value::Null);
            a.backend.send(Command::SleepTimer(Some(Sleep::Minutes(1))));
        }),
        wait("timer set", 5.0, |a| {
            sleep_choice(a) == Some(Sleep::Minutes(1))
        }),
        Step::Sleep(2.0),
        Step::Screenshot("e06-sleep-timer"),
        wait("faded out and paused", 90.0, |a| {
            !a.playback.playing && a.playback.sleep.is_none()
        }),
        wait("paused by the timer, volume back", 5.0, |a| {
            probed("sleep_stopped")["mpv_pause"] == true
                && probed("sleep_stopped")["mpv_volume"].as_f64() == Some(a.playback.volume)
        }),
        measure("sleep_fade_volumes", |_| probed("sleep_fade")),
        measure("sleep_stopped", |_| probed("sleep_stopped")),
        // A session for the next launch: paused at 42 s with volume 63.
        run("volume 63, then 42 s into the song", |a| {
            a.backend.send(Command::Volume(63.0));
            a.backend.send(Command::Seek(42.0));
        }),
        wait("at 42 s with volume 63", 15.0, |a| {
            (a.playback.position - 42.0).abs() < 1.0 && a.playback.volume == 63.0
        }),
        wait(
            "session.json has the queue, song, position and volume",
            15.0,
            session_saved,
        ),
        run("write what engine-restore expects", |a| {
            let mut want = session_state(a);
            want["original"] = probed("engine:original");
            if let Some(file) = expected_file() {
                let _ = std::fs::write(file, serde_json::to_vec_pretty(&want).unwrap_or_default());
            }
        }),
        measure("expected_after_relaunch", |_| expected()),
    ]
}

/// The launch after `engine`: the queue, song, position, volume, shuffle,
/// repeat, autoplay and equalizer are back, paused, at once; Play starts
/// from the saved place without waiting for a stream. Then it puts back
/// the volume, equalizer and levelling the account's owner had before
/// `engine`.
fn engine_restore() -> Vec<Step> {
    vec![
        wait("a restored queue", 10.0, |a| {
            !a.queue.is_empty() && a.playback.index.is_some()
        }),
        measure("restored_after_launch_ms", |a| {
            json!(a.started.elapsed().as_millis() as u64)
        }),
        measure("restored", session_state),
        measure("expected", |_| expected()),
        wait("restored as engine left it, paused", 1.0, |a| {
            restored_as_expected(a) && !a.playback.playing
        }),
        Step::Sleep(2.0),
        Step::Screenshot("r01-restored-paused"),
        wait("signed in", 60.0, |a| {
            matches!(a.account, Account::SignedIn { .. })
        }),
        wait("the restored song prepared", 120.0, |a| {
            current_id(a)
                .as_str()
                .is_some_and(|id| a.backend.prepared(id))
        }),
        mark("play"),
        click("Play"),
        wait("playing from the saved place", 30.0, |a| {
            audible(a)
                && expected()["position"]
                    .as_f64()
                    .is_some_and(|p| a.playback.position >= p - 0.5)
        }),
        measure("play_start_ms", ms_since("play")),
        measure("playing", playing_track),
        Step::Screenshot("r02-playing"),
        run("pause, and put back what engine changed", |a| {
            if a.playback.playing {
                a.backend.send(Command::TogglePause);
            }
            let original = &expected()["original"];
            if let Some(volume) = original["volume"].as_f64() {
                a.backend.send(Command::Volume(volume));
            }
            if let Ok(equalizer) = serde_json::from_value(original["equalizer"].clone()) {
                a.backend.send(Command::Equalizer(equalizer));
            }
            if let Some(on) = original["normalize"].as_bool() {
                a.backend.send(Command::Normalize(on));
            }
        }),
        wait("settings put back", 10.0, |a| {
            let original = &expected()["original"];
            original["volume"].as_f64() == Some(a.playback.volume)
                && original["equalizer"] == json!(a.playback.equalizer)
        }),
        // The equalizer is saved once edits settle.
        Step::Sleep(1.5),
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
