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

/// When the run's clock started, so samples taken in the backend line up
/// with the scenario's own marks.
static CLOCK: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

/// Milliseconds on the run's clock.
pub fn clock_ms() -> u64 {
    CLOCK.elapsed().as_millis() as u64
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
        "motion" => motion(),
        "pages" => pages(),
        "desktop" => desktop(),
        "account" => account(),
        "engine" => engine(),
        "engine-restore" => engine_restore(),
        "surfaces" => surfaces(),
        "deck" => deck(),
        _ => journey(),
    }
}

/// Frames taken back to back (about every other frame) while something moves.
fn burst(names: &'static [&'static str]) -> Vec<Step> {
    names.iter().map(|n| Step::Screenshot(n)).collect()
}

/// The signature motion: a cover flies into its page and back, a song's
/// cover flies to the player, the player hands off to the next song, and
/// Now Playing opens from and closes into the player's cover.
fn motion() -> Vec<Step> {
    let home = View::Home.target();
    let home2 = home.clone();
    let mut steps = vec![
        wait("home loaded", 60.0, move |a| loaded(a, &home, 2)),
        Step::Sleep(3.0),
        Step::Screenshot("00-home"),
        click_with("an album or playlist card on Home", move |a| {
            first_item_title(a, &home2, |i| {
                matches!(
                    i.kind,
                    crate::model::ItemKind::Album | crate::model::ItemKind::Playlist
                ) && i.thumbnail.is_some()
            })
        }),
    ];
    steps.extend(burst(&[
        "01-open-a",
        "01-open-b",
        "01-open-c",
        "01-open-d",
        "01-open-e",
        "01-open-f",
    ]));
    steps.extend([
        wait("page", 60.0, current_loaded),
        Step::Sleep(1.0),
        Step::Screenshot("02-page"),
        click("Back"),
    ]);
    steps.extend(burst(&[
        "03-back-a",
        "03-back-b",
        "03-back-c",
        "03-back-d",
        "03-back-e",
    ]));
    let home3 = View::Home.target();
    steps.extend([
        Step::Sleep(1.5),
        click_first_visible("a song on Home from a shelf of songs", move |a| {
            // A shelf with several songs, so Next has a song ready to hand off to.
            let Some(page) = a.page_state(&home3).and_then(|s| s.page.as_ref()) else {
                return Vec::new();
            };
            page.shelves
                .iter()
                .filter(|s| s.items.iter().filter(|i| i.track.is_some()).count() >= 4)
                .flat_map(|s| &s.items)
                .filter(|i| i.track.is_some() && i.thumbnail.is_some())
                .map(|i| i.title.clone())
                .collect()
        }),
    ]);
    steps.extend(burst(&[
        "04-play-a",
        "04-play-b",
        "04-play-c",
        "04-play-d",
        "04-play-e",
    ]));
    steps.extend([
        wait("playing", 60.0, |a| a.playback.playing),
        Step::Sleep(2.0),
        Step::Screenshot("05-playing"),
        click("Next"),
    ]);
    steps.extend(burst(&[
        "06-handoff-a",
        "06-handoff-b",
        "06-handoff-c",
        "06-handoff-d",
        "06-handoff-e",
    ]));
    steps.extend([Step::Sleep(1.0), click("Cover")]);
    steps.extend(burst(&[
        "07-np-open-a",
        "07-np-open-b",
        "07-np-open-c",
        "07-np-open-d",
        "07-np-open-e",
    ]));
    steps.extend([
        Step::Sleep(1.0),
        Step::Screenshot("08-now-playing"),
        click("Close player"),
    ]);
    steps.extend(burst(&[
        "09-np-close-a",
        "09-np-close-b",
        "09-np-close-c",
        "09-np-close-d",
    ]));
    // Physical feel: pause/play morph, and a carousel gliding to a card edge.
    steps.extend([Step::Sleep(1.5), click("Pause")]);
    steps.extend(burst(&["10-morph-a", "10-morph-b", "10-morph-c"]));
    let home4 = View::Home.target();
    steps.extend([
        Step::Sleep(1.0),
        click_with("the first carousel's scroll-right arrow", move |a| {
            let page = a.page_state(&home4)?.page.as_ref()?;
            page.shelves
                .iter()
                .find(|s| s.style == crate::model::ShelfStyle::Carousel && s.items.len() > 8)
                .map(|s| format!("{}, scroll right", s.title))
        }),
    ]);
    steps.extend(burst(&["11-glide-a", "11-glide-b", "11-glide-c"]));
    steps.extend([Step::Sleep(1.0), Step::Screenshot("11-glide-settled")]);
    steps.extend([
        Step::Sleep(1.0),
        run("pause", |a| {
            if a.playback.playing {
                a.backend.send(crate::backend::Command::TogglePause);
            }
        }),
        Step::Sleep(1.0),
    ]);
    steps
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
            matches!(a.current_lyrics(), Some(Ok(Some(_))))
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

/// A public YouTube Music playlist (override with `YTFAST_E2E_PLAYLIST`).
fn public_playlist() -> String {
    std::env::var("YTFAST_E2E_PLAYLIST")
        .unwrap_or_else(|_| "RDCLAK5uy_k6ACq4WNfG-uJSz_jML9ZkUEULUoCzWIw".into())
}

/// Runs a program to completion: its exit status and output.
fn exec(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Value {
    match std::process::Command::new(program).args(args).output() {
        Ok(output) => json!({
            "args": args,
            "status": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout).trim(),
            "stderr": String::from_utf8_lossy(&output.stderr).trim(),
        }),
        Err(error) => json!({"args": args, "error": error.to_string()}),
    }
}

/// The binary under test, driving itself as a second launch would.
fn ytfast_command(args: &[&str]) -> Value {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ytfast"));
    exec(exe, args)
}

fn playerctl(args: &[&str]) -> Value {
    let mut all = vec!["-p", "ytfast"];
    all.extend_from_slice(args);
    exec("playerctl", &all)
}

fn stdout(value: &Value) -> String {
    value["stdout"].as_str().unwrap_or_default().to_owned()
}

/// A song id or page id remembered by one step for a later one.
static MARK: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

fn set_mark(value: String) {
    if let Ok(mut mark) = MARK.lock() {
        *mark = value;
    }
}

fn mark() -> String {
    MARK.lock().map(|m| m.clone()).unwrap_or_default()
}

fn playing_id(app: &App) -> Option<String> {
    app.playback
        .index
        .and_then(|i| app.queue.get(i))
        .map(|t| t.video_id.clone())
}

/// Another song than the marked one is playing.
fn song_changed(app: &App) -> bool {
    app.playback.playing && playing_id(app).is_some_and(|id| id != mark())
}

/// `check`, at most once a second (it runs a program).
fn every_second(check: impl Fn(&App) -> bool + 'static) -> impl Fn(&App) -> bool + 'static {
    let last = std::cell::Cell::new(None::<Instant>);
    let passed = std::cell::Cell::new(false);
    move |app| {
        if last
            .get()
            .is_none_or(|t| t.elapsed() > Duration::from_secs(1))
        {
            last.set(Some(Instant::now()));
            passed.set(check(app));
        }
        passed.get()
    }
}

/// The playing song's first artist page, as a music.youtube.com link.
fn artist_link(app: &App) -> Option<String> {
    let track = app.playback.index.and_then(|i| app.queue.get(i))?;
    track.artists.iter().find_map(|run| match &run.target {
        Some(Target::Browse { id, .. }) if id.starts_with("UC") => {
            Some(format!("https://music.youtube.com/channel/{id}"))
        }
        _ => None,
    })
}

/// The desktop journey (SPEC § Completion evidence 8): MPRIS through
/// `playerctl`, playing on with the window closed, the command line
/// (`ytfast show|next|open`) on the binary under test, a pasted link, the
/// mini player, and a song-change notification while the window is closed.
/// Leaves notifications off, as they are by default, and playback paused.
fn desktop() -> Vec<Step> {
    let home = View::Home.target();
    let playlist = public_playlist();
    let playlist_page = Target::browse(format!("VL{playlist}"));
    vec![
        wait("home loaded", 60.0, move |a| loaded(a, &home, 1)),
        run("play a public playlist", move |a| {
            a.backend.send(Command::PlayTarget(Target::Watch {
                video_id: None,
                playlist_id: Some(public_playlist()),
                params: None,
            }))
        }),
        wait("playing", 90.0, |a| {
            a.playback.playing && a.playback.position > 1.0
        }),
        measure("first_song", playing_track),
        // MPRIS, read and driven by playerctl.
        wait(
            "playerctl shows the song, playing",
            15.0,
            every_second(|a| {
                let title = stdout(&playerctl(&["metadata", "xesam:title"]));
                let status = stdout(&playerctl(&["status"]));
                let track = a.playback.index.and_then(|i| a.queue.get(i));
                status == "Playing" && track.is_some_and(|t| t.title == title)
            }),
        ),
        measure("playerctl_metadata", |_| playerctl(&["metadata"])),
        measure("playerctl_status", |_| playerctl(&["status"])),
        measure("playerctl_position", |_| playerctl(&["position"])),
        run("mark the song", |a| {
            set_mark(playing_id(a).unwrap_or_default())
        }),
        measure("playerctl_next", |_| playerctl(&["next"])),
        wait("playerctl next changed the song", 60.0, song_changed),
        measure("after_playerctl_next", playing_track),
        // Notifications on, through Settings.
        click("Settings"),
        click("Song notifications"),
        wait("notifications on", 10.0, |a| {
            a.desktop
                .notifications
                .load(std::sync::atomic::Ordering::Relaxed)
        }),
        Step::Key(egui::Key::Escape),
        Step::Sleep(1.0),
        // Closing the window plays on.
        Step::Window("close the window", egui::ViewportCommand::Close),
        wait("window closed, still playing", 15.0, |a| {
            a.hidden && a.playback.playing
        }),
        Step::Sleep(3.0),
        measure("playing_while_closed", playing_track),
        measure("status_while_closed", |_| playerctl(&["status"])),
        // A song change with no window focused: a notification.
        run("mark the song", |a| {
            set_mark(playing_id(a).unwrap_or_default())
        }),
        measure("ytfast_next_while_closed", |_| ytfast_command(&["next"])),
        wait("song changed while closed", 60.0, song_changed),
        wait("notification sent", 20.0, |a| {
            let track = a.playback.index.and_then(|i| a.queue.get(i));
            crate::notify::last_sent()
                .is_some_and(|(_, title)| track.is_some_and(|t| t.title == title))
        }),
        measure("notification", |_| {
            json!(crate::notify::last_sent().map(|(id, title)| json!({"id": id, "title": title})))
        }),
        // `ytfast show` brings the window back.
        measure("ytfast_show", |_| ytfast_command(&["show"])),
        wait("window back", 20.0, |a| !a.hidden),
        Step::Sleep(3.0),
        Step::Screenshot("d1-shown-again"),
        run("mark the song", |a| {
            set_mark(playing_id(a).unwrap_or_default())
        }),
        measure("ytfast_next", |_| ytfast_command(&["next"])),
        wait("ytfast next changed the song", 60.0, song_changed),
        measure("after_ytfast_next", playing_track),
        // `ytfast open <link>` opens the playlist's page.
        measure("ytfast_open", move |_| {
            ytfast_command(&[
                "open",
                &format!("https://music.youtube.com/playlist?list={playlist}"),
            ])
        }),
        wait("linked playlist page", 60.0, move |a| {
            a.view == View::Page(playlist_page.clone()) && current_loaded(a)
        }),
        Step::Sleep(2.0),
        Step::Screenshot("d2-opened-link"),
        // A link pasted into search opens its page.
        wait("an artist link", 30.0, |a| artist_link(a).is_some()),
        run("remember the artist", |a| {
            set_mark(artist_link(a).unwrap_or_default())
        }),
        click("Search"),
        Step::Paste(Box::new(artist_link)),
        wait("pasted artist page", 60.0, |a| {
            let wanted = mark();
            matches!(&a.view, View::Page(Target::Browse { id, .. }) if wanted.ends_with(id.as_str()))
                && current_loaded(a)
        }),
        Step::Sleep(2.0),
        Step::Screenshot("d3-pasted-link"),
        // The mini player: its own window, driving the same session.
        click("Mini player"),
        wait("mini player open", 20.0, |a| {
            a.window == crate::app::WindowKind::Mini && !a.hidden
        }),
        Step::Sleep(3.0),
        Step::Screenshot("d4-mini-player"),
        run("mark the song", |a| {
            set_mark(playing_id(a).unwrap_or_default())
        }),
        click("Next"),
        wait(
            "the mini player's Next changed the song",
            60.0,
            song_changed,
        ),
        Step::Sleep(1.0),
        Step::Screenshot("d5-mini-player-next"),
        click("Full player"),
        wait("full window again", 20.0, |a| {
            a.window == crate::app::WindowKind::Main && !a.hidden
        }),
        Step::Sleep(2.0),
        Step::Screenshot("d6-full-window"),
        // Leave things as they were: notifications off, paused.
        run("notifications off, pause", |a| {
            a.desktop
                .notifications
                .store(false, std::sync::atomic::Ordering::Relaxed);
            a.backend.send(Command::Notifications(false));
            if a.playback.playing {
                a.backend.send(Command::TogglePause);
            }
        }),
        wait(
            "playerctl shows paused",
            15.0,
            every_second(|_| stdout(&playerctl(&["status"])) == "Paused"),
        ),
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
type Candidates = Box<dyn Fn(&App) -> Vec<String>>;
type Measure = Box<dyn Fn(&App) -> Value>;
type Run = Box<dyn Fn(&mut App)>;

enum Step {
    Wait {
        what: &'static str,
        timeout: f64,
        check: Check,
    },
    Click {
        /// Names to click; the first one visible this frame is used.
        label: Candidates,
        describe: String,
        timeout: f64,
    },
    Type(String),
    /// Pastes text (as Ctrl+V would) into the focused field.
    Paste(Label),
    /// Asks the window for something, as the user or compositor would.
    Window(&'static str, egui::ViewportCommand),
    Key(egui::Key),
    /// Holds modifier keys down from now on (as held keys report them);
    /// `Modifiers::NONE` lets go.
    Hold(egui::Modifiers),
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
    /// Moves the pointer off the window.
    Leave,
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
        label: Box::new(move |_| vec![fixed.clone()]),
        timeout: 15.0,
    }
}

fn click_with(describe: &str, label: impl Fn(&App) -> Option<String> + 'static) -> Step {
    Step::Click {
        describe: describe.to_owned(),
        label: Box::new(move |a| label(a).into_iter().collect()),
        timeout: 20.0,
    }
}

/// Clicks the first of several candidates that is on screen (Home is
/// personal and changes between runs; carousels hide most of their items).
fn click_first_visible(describe: &str, labels: impl Fn(&App) -> Vec<String> + 'static) -> Step {
    Step::Click {
        describe: describe.to_owned(),
        label: Box::new(labels),
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

/// The display's refresh rate as scripts/e2e.sh found it (60 if unknown).
fn refresh_hz() -> f32 {
    std::env::var("YTFAST_E2E_REFRESH_HZ")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|hz| *hz > 1.0)
        .unwrap_or(60.0)
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

fn item_titles(
    app: &App,
    target: &Target,
    pick: impl Fn(&crate::model::Item) -> bool,
) -> Vec<String> {
    app.page_state(target)
        .and_then(|s| s.page.as_ref())
        .map(|page| {
            page.shelves
                .iter()
                .flat_map(|s| &s.items)
                .filter(|i| pick(i))
                .map(|i| i.title.clone())
                .collect()
        })
        .unwrap_or_default()
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
static CONFIRMED: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

fn fact(key: &str) -> Option<String> {
    FACTS.lock().expect("facts lock").get(key).cloned()
}

fn set_fact(key: &'static str, value: String) {
    FACTS.lock().expect("facts lock").insert(key, value);
}

fn set_confirmed() {
    *CONFIRMED.lock().expect("confirmed lock") = Some(Instant::now());
}

/// The page as fetched from YouTube Music after the last mark.
fn refetched<'a>(app: &'a App, target: &Target) -> Option<&'a crate::model::Page> {
    let mark = (*CONFIRMED.lock().expect("confirmed lock"))?;
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

/// The playlist's own songs, not the "Suggestions" YouTube Music lists after them.
fn song_ids(page: &crate::model::Page) -> Vec<String> {
    crate::account::entries(page)
        .map(|s| {
            s.items
                .iter()
                .filter_map(|i| i.track.as_ref().map(|t| t.video_id.clone()))
                .collect()
        })
        .unwrap_or_default()
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
    set_confirmed();
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
        // Once YouTube Music's own pages speak for the song again, the
        // player bar still shows the like: Liked Music lists it, and the
        // queue's copy of the song carries no rating of its own.
        Step::Sleep(16.0),
        run("fetch Liked Music", |a| {
            set_confirmed();
            a.ensure_page(Target::browse("VLLM"), true);
        }),
        wait("Liked Music fetched", 60.0, |a| {
            refetched(a, &Target::browse("VLLM")).is_some()
        }),
        wait("player bar still shows the like", 1.0, |a| {
            a.playback
                .index
                .and_then(|i| a.queue.get(i))
                .map(|t| a.account_state.marks.like(t))
                == Some(crate::model::LikeStatus::Like)
        }),
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
        run("mark", |_| set_confirmed()),
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
        run("mark", |_| set_confirmed()),
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
        run("mark", |_| set_confirmed()),
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
        run("mark", |_| set_confirmed()),
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
            set_confirmed();
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
            set_confirmed();
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
        run("mark", |_| set_confirmed()),
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
        run("mark", |_| set_confirmed()),
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
        run("mark", |_| set_confirmed()),
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
        run("mark", |_| set_confirmed()),
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
        measure("lyrics", |a| json!(lyrics_state(a))),
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
        // An album from Explore's new releases (Home is personal and its
        // albums are often off screen inside a carousel).
        click("Explore"),
        wait("explore again", 60.0, |a| {
            a.view == View::Explore && loaded(a, &View::Explore.target(), 2)
        }),
        click_first_visible("an album on Explore", |a| {
            item_titles(a, &View::Explore.target(), |i| {
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

/// How far the playing song's lyrics have come: timed, plain, none, error or loading.
fn lyrics_state(app: &App) -> &'static str {
    match app.current_lyrics() {
        Some(Ok(Some(l))) if !l.lines.is_empty() => "timed",
        Some(Ok(Some(_))) => "plain",
        Some(Ok(None)) => "none",
        Some(Err(_)) => "error",
        None => "loading",
    }
}

fn timed_lines(app: &App) -> Option<&[crate::model::LyricLine]> {
    match app.current_lyrics() {
        Some(Ok(Some(l))) if !l.lines.is_empty() => Some(&l.lines),
        _ => None,
    }
}

/// The lyric line lit now, as the Lyrics tab draws it.
fn lyric_index(app: &App) -> Option<usize> {
    crate::lyrics::current_line(timed_lines(app)?, app.position_now())
}

/// The playing cover's colours are worked out (not the previous song's).
fn cover_colours_ready(app: &App) -> bool {
    let cover = app.current_track().and_then(|t| t.thumbnail.as_deref());
    cover.is_some() && app.cover_url.as_deref() == cover
}

/// The cover's extracted colours and what Now Playing makes of them under
/// the current theme, with contrast ratios against the wash's two ends.
fn cover_record(app: &App) -> Value {
    let hex = |c: egui::Color32| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
    let wash = crate::colors::Wash::new(app.cover_colors.as_ref(), &app.palette);
    let worst = |c: egui::Color32| {
        crate::colors::contrast(c, wash.top).min(crate::colors::contrast(c, wash.bottom))
    };
    let track = app.current_track();
    json!({
        "video_id": track.map(|t| t.video_id.clone()),
        "title": track.map(|t| t.title.clone()),
        "theme_dark": app.palette.dark,
        "deep": app.cover_colors.map(|c| hex(c.deep)),
        "accent": app.cover_colors.map(|c| hex(c.accent)),
        "neutral": app.cover_colors.map(|c| c.neutral),
        "wash_top": hex(wash.top),
        "wash_bottom": hex(wash.bottom),
        "text": hex(wash.text),
        "accent_shown": hex(wash.accent),
        "contrast_text": worst(wash.text),
        "contrast_secondary": worst(wash.secondary),
        "contrast_accent": worst(wash.accent),
    })
}

fn page_of<'a>(app: &'a App, target: &Target) -> Option<&'a crate::model::Page> {
    app.page_state(target)?.page.as_ref()
}

/// A Home mood (Energize, Relax…) is open and loaded, its chip selected.
fn mood_open(app: &App) -> bool {
    matches!(&app.view, View::Page(Target::Browse { id, params: Some(_) }) if id == "FEmusic_home")
        && current_loaded(app)
        && page_of(app, &app.view.target()).is_some_and(|p| p.chips.iter().any(|c| c.selected))
}

/// The first song on the current page whose title contains `want` (any
/// song if none does).
fn song_on_page(app: &App, want: &str) -> Option<crate::model::Track> {
    let page = page_of(app, &app.view.target())?;
    let songs: Vec<&crate::model::Track> = page
        .shelves
        .iter()
        .flat_map(|s| &s.items)
        .filter(|i| i.kind == crate::model::ItemKind::Song)
        .filter_map(|i| i.track.as_ref())
        .collect();
    songs
        .iter()
        .find(|t| t.title.contains(want))
        .or(songs.first())
        .map(|t| (*t).clone())
}

fn searched(app: &App) -> bool {
    matches!(&app.view, View::Page(Target::Search { .. })) && current_loaded(app)
}

/// Now Playing and pages (docs/SPEC.md § Now Playing and pages): Home's mood
/// chips, Library → History, an artist's See all, recent searches, the cover
/// wash for two different covers under the current and a light theme, timed
/// lyrics following the song and seeking by line, and a song without timed
/// lyrics. Restores the theme and the recent searches it found.
fn pages() -> Vec<Step> {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    let home = View::Home.target();
    let original = omarchy_theme().unwrap_or_else(|| "Permafrost".into());
    let light = std::env::var("YTFAST_E2E_LIGHT_THEME").unwrap_or_else(|_| "Snow".into());
    let saved: Rc<RefCell<Vec<String>>> = Rc::default();
    let song_a: Rc<RefCell<Option<crate::model::Track>>> = Rc::default();
    let song_b: Rc<RefCell<Option<crate::model::Track>>> = Rc::default();
    let first_line: Rc<Cell<Option<usize>>> = Rc::default();
    let jump: Rc<Cell<f64>> = Rc::default();
    let (saved_1, saved_2) = (saved.clone(), saved);
    let (a1, a2, a3, a4) = (song_a.clone(), song_a.clone(), song_a.clone(), song_a);
    let (b1, b2, b3, b4) = (song_b.clone(), song_b.clone(), song_b.clone(), song_b);
    let (l1, l2) = (first_line.clone(), first_line);
    let (j1, j2, j3) = (jump.clone(), jump.clone(), jump);
    let clear_search = || run("clear the search field", |a| a.search.clear());
    let search = |query: &str| {
        vec![
            clear_search(),
            click("Search"),
            Step::Type(query.into()),
            Step::Key(egui::Key::Enter),
            wait("search results", 60.0, searched),
        ]
    };
    let mut steps = vec![
        wait("signed in", 60.0, |a| {
            matches!(a.account, Account::SignedIn { .. })
        }),
        wait("home loaded", 60.0, move |a| loaded(a, &home, 2)),
        run("note the recent searches", move |a| {
            *saved_1.borrow_mut() = a.recent_searches.clone();
        }),
        // Home's mood chips: one chosen, then chosen again to go back.
        wait("home mood chips", 30.0, |a| {
            page_of(a, &View::Home.target()).is_some_and(|p| p.chips.len() >= 3)
        }),
        measure("home_chips", |a| {
            json!(
                page_of(a, &View::Home.target()).map(|p| p
                    .chips
                    .iter()
                    .map(|c| c.text.clone())
                    .collect::<Vec<_>>())
            )
        }),
        Step::Sleep(2.0),
        Step::Screenshot("p01-home-chips"),
        click_with("a mood chip", |a| {
            page_of(a, &View::Home.target())?
                .chips
                .iter()
                .find(|c| !c.selected && c.target.is_some() && c.text != "Podcasts")
                .map(|c| c.text.clone())
        }),
        wait("mood page", 60.0, mood_open),
        Step::Sleep(3.0),
        Step::Screenshot("p02-mood"),
        measure("mood", |a| {
            let page = page_of(a, &a.view.target());
            json!({
                "selected": page.and_then(|p| p.chips.iter().find(|c| c.selected)).map(|c| c.text.clone()),
                "shelves": page.map(|p| p.shelves.iter().map(|s| s.title.clone()).collect::<Vec<_>>()),
            })
        }),
        click_with("the selected mood chip", |a| {
            page_of(a, &a.view.target())?
                .chips
                .iter()
                .find(|c| c.selected)
                .map(|c| c.text.clone())
        }),
        wait("back on Home", 30.0, |a| a.view == View::Home),
        Step::Sleep(2.0),
        Step::Screenshot("p03-home-again"),
        // Library → History, with day headings.
        click("Library"),
        wait("library", 60.0, current_loaded),
        click("History"),
        wait("history", 60.0, |a| {
            a.view == View::Library(LibraryTab::History) && current_loaded(a)
        }),
        Step::Sleep(3.0),
        Step::Screenshot("p04-history"),
        measure("history_days", |a| {
            json!(page_of(a, &a.view.target()).map(|p| {
                p.shelves
                    .iter()
                    .map(|s| json!({"heading": s.title, "rows": s.items.len()}))
                    .collect::<Vec<_>>()
            }))
        }),
    ];
    // An artist's See all (Albums), from the first search.
    steps.extend(search("Coldplay"));
    steps.extend([
        run("note song A", move |a| *a1.borrow_mut() = song_on_page(a, "Yellow")),
        run("open the artist", |a| {
            let artist = |i: &crate::model::Item| i.kind == crate::model::ItemKind::Artist;
            if !open_item(a, |i| artist(i) && i.title == "Coldplay") {
                open_item(a, artist);
            }
        }),
        wait("artist page", 60.0, artist_or_album),
        Step::Sleep(3.0),
        Step::Screenshot("p05-artist"),
        measure("artist_see_all", |a| {
            json!(page_of(a, &a.view.target()).map(|p| p
                .shelves
                .iter()
                .filter(|s| s.more.is_some())
                .map(|s| s.title.clone())
                .collect::<Vec<_>>()))
        }),
        // As its More button does (the shelf may be below the fold).
        run("open the Albums shelf's See all", |a| {
            let more = page_of(a, &a.view.target()).and_then(|p| {
                p.shelves
                    .iter()
                    .find(|s| s.title == "Albums" && s.more.is_some())
                    .or_else(|| p.shelves.iter().find(|s| s.more.is_some()))
                    .and_then(|s| s.more.clone())
            });
            if let Some(target) = more {
                a.open(View::Page(target));
            }
        }),
        wait("See all grid", 60.0, |a| {
            current_loaded(a)
                && page_of(a, &a.view.target()).is_some_and(|p| {
                    p.shelves
                        .iter()
                        .any(|s| s.style == crate::model::ShelfStyle::Grid)
                })
        }),
        Step::Sleep(3.0),
        Step::Screenshot("p06-see-all-albums"),
        measure("see_all", |a| {
            let page = page_of(a, &a.view.target());
            json!({
                "title": page.and_then(|p| p.header.as_ref()).map(|h| h.title.clone()),
                "chips": page.map(|p| p.chips.iter().map(|c| (c.text.clone(), c.selected)).collect::<Vec<_>>()),
                "cards": page.map(|p| p.shelves.iter().filter(|s| s.style == crate::model::ShelfStyle::Grid).map(|s| s.items.len()).sum::<usize>()),
            })
        }),
    ]);
    steps.extend(search("Daft Punk Get Lucky"));
    steps.extend([
        run("note song B", move |a| {
            *b1.borrow_mut() = song_on_page(a, "Get Lucky")
        }),
        // Recent searches: the field focused and empty.
        clear_search(),
        click("Search"),
        Step::Sleep(1.0),
        Step::Screenshot("p07-recent-searches"),
        measure("recent_searches", |a| {
            json!(a.recent_searches.iter().take(5).collect::<Vec<_>>())
        }),
        Step::Key(egui::Key::Escape),
        // Two songs with different covers.
        run("play songs A and B", move |a| {
            let tracks: Vec<_> = [a2.borrow().clone(), b2.borrow().clone()]
                .into_iter()
                .flatten()
                .collect();
            a.backend.send(Command::PlayTracks { tracks, start: 0 });
        }),
        wait("song A playing", 90.0, |a| {
            a.playback.index == Some(0) && a.playback.playing && a.playback.position > 0.5
        }),
        click("Open player"),
        wait("now playing", 10.0, |a| a.now_playing),
        wait("cover A colours", 30.0, cover_colours_ready),
        Step::Sleep(1.5),
        Step::Screenshot("p08-now-playing-a"),
        measure("cover_a", cover_record),
        click_with("song B in Up next", move |_| {
            b3.borrow().as_ref().map(|t| t.title.clone())
        }),
        wait("song B playing", 90.0, |a| {
            a.playback.index == Some(1) && a.playback.playing && a.playback.position > 0.5
        }),
        wait("cover B colours", 30.0, cover_colours_ready),
        Step::Sleep(1.5),
        Step::Screenshot("p09-now-playing-b"),
        measure("cover_b", cover_record),
        // Timed lyrics follow the song.
        click("LYRICS"),
        wait("timed lyrics", 45.0, |a| timed_lines(a).is_some()),
        measure("lyrics", |a| {
            json!({
                "state": lyrics_state(a),
                "source": match a.current_lyrics() {
                    Some(Ok(Some(l))) => l.source.clone(),
                    _ => None,
                },
                "lines": timed_lines(a).map(<[_]>::len),
            })
        }),
        run("seek to a minute in", |a| a.backend.send(Command::Seek(60.0))),
        wait("at a minute", 15.0, |a| {
            a.playback.position >= 60.0 && a.playback.position < 75.0 && a.playback.playing
        }),
        Step::Sleep(2.0),
        measure("line_first", move |a| {
            l1.set(lyric_index(a));
            json!({"index": lyric_index(a), "position": a.position_now()})
        }),
        Step::Sleep(10.0),
        measure("line_second", |a| {
            json!({"index": lyric_index(a), "position": a.position_now()})
        }),
        wait("the lit line moved on", 5.0, move |a| {
            matches!((l2.get(), lyric_index(a)), (Some(first), Some(now)) if now > first)
        }),
        Step::Screenshot("p10-lyrics-timed"),
        click_with("a later lyric line", move |a| {
            let lines = timed_lines(a)?;
            let now = lyric_index(a)?;
            let pick = (now + 3..(now + 8).min(lines.len())).find(|&j| {
                let text = &lines[j].text;
                !text.is_empty() && lines.iter().filter(|l| l.text == *text).count() == 1
            })?;
            j1.set(lines[pick].start);
            Some(lines[pick].text.clone())
        }),
        wait("seeked to the line", 10.0, move |a| {
            let start = j2.get();
            a.playback.position >= start - 0.5 && a.playback.position < start + 3.0
        }),
        measure("after_line_click", move |a| {
            json!({"line_start": j3.get(), "position": a.playback.position, "index": lyric_index(a)})
        }),
        // The same under a light theme.
        run("switch to a light theme", move |_| set_theme(&light)),
        wait("light colours", 120.0, |a| !a.palette.dark),
        Step::Sleep(3.0),
        Step::Screenshot("p11-lyrics-light"),
        click("UP NEXT"),
        Step::Sleep(1.0),
        Step::Screenshot("p12-now-playing-b-light"),
        measure("cover_b_light", cover_record),
        click_with("song A in Up next", move |_| {
            a3.borrow().as_ref().map(|t| t.title.clone())
        }),
        wait("song A again", 90.0, |a| {
            a.playback.index == Some(0) && a.playback.playing && a.playback.position > 0.5
        }),
        wait("cover A colours again", 30.0, cover_colours_ready),
        Step::Sleep(1.5),
        Step::Screenshot("p13-now-playing-a-light"),
        measure("cover_a_light", cover_record),
        run("switch the theme back", move |_| set_theme(&original)),
        wait("dark colours", 120.0, |a| a.palette.dark),
        Step::Sleep(2.0),
        click("Close player"),
    ]);
    // A song without timed lyrics: plain lyrics, or the message.
    steps.extend(search("Debussy Clair de Lune"));
    steps.extend([
        click_with("a song's cover", |a| {
            song_on_page(a, "Clair de Lune").map(|t| format!("Play {}", t.title))
        }),
        wait("another song playing", 90.0, move |a| {
            let id = a.current_track().map(|t| t.video_id.clone());
            let ours = |s: &Rc<RefCell<Option<crate::model::Track>>>| {
                s.borrow().as_ref().map(|t| t.video_id.clone())
            };
            id.is_some()
                && id != ours(&a4)
                && id != ours(&b4)
                && a.playback.playing
                && a.playback.position > 0.5
        }),
        click("Open player"),
        click("LYRICS"),
        wait("lyrics answered", 45.0, |a| lyrics_state(a) != "loading"),
        Step::Sleep(2.0),
        Step::Screenshot("p14-untimed-lyrics"),
        measure("untimed_lyrics", |a| {
            json!({"title": a.current_track().map(|t| t.title.clone()), "state": lyrics_state(a)})
        }),
        click("Close player"),
        run("restore the recent searches", move |a| {
            a.recent_searches = saved_2.borrow().clone();
            a.backend
                .send(Command::SaveSearches(a.recent_searches.clone()));
        }),
        run("pause", |a| {
            if a.playback.playing {
                a.backend.send(Command::TogglePause);
            }
        }),
    ]);
    steps
}

// ---- engine: playback control ----

/// Moments the engine scenarios time from.
static MARKS: std::sync::Mutex<BTreeMap<&'static str, Instant>> =
    std::sync::Mutex::new(BTreeMap::new());

fn mark_time(name: &'static str) -> Step {
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

/// Since when the song playing has been current, for [`advance_through`].
static CURRENT_SINCE: std::sync::Mutex<Option<(String, Instant)>> = std::sync::Mutex::new(None);

/// Lets the queue play on: notes each song as it plays and seeks near its
/// end so the next one follows (gapless, as queued in mpv). A song whose
/// length mpv never reported can't be sought to its end; after 20 s it is
/// skipped with Next, and `engine:advances` says so.
fn advance_through(app: &mut App) {
    let mut played: Vec<String> =
        serde_json::from_value(probed("engine:played")).unwrap_or_default();
    let Value::String(id) = current_id(app) else {
        return;
    };
    if played.last() != Some(&id) {
        played.push(id.clone());
        probe("engine:played", json!(played));
    }
    let since = {
        let mut current = CURRENT_SINCE.lock().expect("current lock");
        if current.as_ref().is_none_or(|(c, _)| *c != id) {
            *current = Some((id.clone(), Instant::now()));
        }
        current
            .as_ref()
            .map_or(0.0, |(_, t)| t.elapsed().as_secs_f64())
    };
    let pb = &app.playback;
    if !audible(app) {
        return;
    }
    if pb.duration > 10.0 {
        if pb.position < pb.duration - 6.0 {
            probe_push(
                "engine:advances",
                json!({"from": id, "by": "end", "duration": pb.duration}),
            );
            app.backend.send(Command::Seek(pb.duration - 4.0));
        }
    } else if since > 20.0 {
        probe_push(
            "engine:advances",
            json!({"from": id, "by": "next", "duration": pb.duration}),
        );
        CURRENT_SINCE.lock().expect("current lock").take();
        app.backend.send(Command::Next);
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
        mark_time("page"),
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
        mark_time("cold"),
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
        mark_time("warm"),
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
            let mut tracks: Vec<crate::model::Track> = a
                .page_state(&View::Home.target())
                .and_then(|s| s.page.as_ref())
                .map(|p| {
                    p.shelves
                        .iter()
                        .flat_map(|s| &s.items)
                        .filter_map(|i| i.track.clone())
                        .collect()
                })
                .unwrap_or_default();
            // Songs of a known, ordinary length first: hour-long mixes take
            // longer to seek through and hold the run up.
            tracks.sort_by_key(|t| match t.duration {
                Some(d) if d <= 420 => 0,
                None => 1,
                Some(_) => 2,
            });
            for track in tracks {
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
        measure("advances", |_| probed("engine:advances")),
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
        mark_time("play"),
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

/// Signature surfaces (docs/SPEC.md § Signature moments): the most-replayed
/// ridge on the player bar at rest and under the pointer, the jump to the
/// most replayed part, Stage (the cover's flight in, timed lyrics, frame
/// times, the chrome fading, the flight back), and theme-painted covers
/// across a switch to a light theme and back. Restores the theme, the
/// setting and the recent searches it found.
fn surfaces() -> Vec<Step> {
    use std::cell::RefCell;
    use std::rc::Rc;
    let home = View::Home.target();
    let original = omarchy_theme().unwrap_or_else(|| "Permafrost".into());
    let light = std::env::var("YTFAST_E2E_LIGHT_THEME").unwrap_or_else(|_| "Snow".into());
    let saved: Rc<RefCell<Vec<String>>> = Rc::default();
    let (saved_1, saved_2) = (saved.clone(), saved);
    let peak_of = |a: &App| a.current_heat().and_then(|h| h.peak);
    let mut steps = vec![
        wait("home loaded", 60.0, move |a| loaded(a, &home, 1)),
        run("note the recent searches", move |a| {
            *saved_1.borrow_mut() = a.recent_searches.clone();
        }),
        run("clear the search field", |a| a.search.clear()),
        click("Search"),
        Step::Type("Daft Punk Get Lucky".into()),
        Step::Key(egui::Key::Enter),
        wait("search results", 60.0, searched),
        click_with("Get Lucky's cover", |a| {
            song_on_page(a, "Get Lucky").map(|t| format!("Play {}", t.title))
        }),
        wait("playing", 90.0, |a| {
            a.playback.playing && a.playback.position > 0.5 && a.playback.duration > 0.0
        }),
        // Most replayed: asked for once per song, anonymously.
        wait("heat answered", 30.0, |a| {
            a.current_track()
                .is_some_and(|t| a.heat.contains_key(&t.video_id))
        }),
        measure("heat", |a| {
            let heat = a.current_heat();
            json!({
                "video_id": a.current_track().map(|t| t.video_id.clone()),
                "title": a.current_track().map(|t| t.title.clone()),
                "markers": heat.map(|h| h.markers.len()),
                "length": heat.map(crate::heat::Heat::length),
                "duration": a.playback.duration,
                "peak": heat.and_then(|h| h.peak).map(|p| json!({"start": p.start, "end": p.end, "at": p.at})),
            })
        }),
        wait("the song has heat", 1.0, move |a| peak_of(a).is_some()),
        Step::Sleep(1.5),
        Step::Screenshot("s01-ridge-rest"),
        hover_with("Seek", |_| Some("Seek".into())),
        Step::Sleep(1.0),
        Step::Screenshot("s02-ridge-hover"),
        Step::Leave,
        Step::Sleep(0.5),
        // The jump to the peak, from Now Playing.
        click("Cover"),
        wait("now playing", 10.0, |a| a.now_playing),
        Step::Sleep(1.0),
        click("Jump to the most replayed part"),
        wait("at the most replayed part", 15.0, move |a| {
            peak_of(a).is_some_and(|p| {
                a.playback.position >= p.start - 0.5 && a.playback.position <= p.end + 0.5
            })
        }),
        measure("jump_to_peak", move |a| {
            json!({
                "position": a.playback.position,
                "peak": peak_of(a).map(|p| json!({"start": p.start, "end": p.end})),
            })
        }),
        Step::Sleep(1.0),
        Step::Screenshot("s03-now-playing-peak"),
        // Stage: the cover flies from Now Playing into it.
        Step::Key(egui::Key::F),
    ];
    steps.extend(burst(&[
        "s04-stage-flight-a",
        "s04-stage-flight-b",
        "s04-stage-flight-c",
        "s04-stage-flight-d",
        "s04-stage-flight-e",
    ]));
    steps.extend([
        wait("stage open", 5.0, |a| a.stage.open),
        wait("timed lyrics", 45.0, |a| timed_lines(a).is_some()),
        measure("stage_lyrics", |a| {
            json!({"state": lyrics_state(a), "lit": lyric_index(a), "position": a.position_now()})
        }),
        // The pointer moves: the chrome shows.
        hover_with("Stage cover", |_| Some("Stage cover".into())),
        Step::Sleep(1.5),
        Step::Screenshot("s05-stage"),
        measure("stage_chrome_shown", |a| json!(a.stage.chrome)),
        run("count Stage's frames from here", |a| a.stage.clear_frames()),
        Step::Sleep(3.0),
        measure("stage_frames", |a| {
            let (mean, worst, frames) = a.stage.frame_times();
            json!({"mean_stable_dt_ms": mean, "worst_stable_dt_ms": worst, "frames": frames})
        }),
        // Every frame on time: the mean within 10 % of the display's frame
        // interval and none longer than one and a half (ibara's virtual
        // display on the OptiPlex runs at 30 Hz; scripts/e2e.sh passes the rate).
        wait("Stage frames on time for the display", 1.0, |a| {
            let (mean, worst, frames) = a.stage.frame_times();
            let interval = 1000.0 / refresh_hz();
            frames > 0 && mean < interval * 1.1 && worst < interval * 1.5
        }),
        // The pointer rests: the chrome fades.
        wait("chrome faded", 10.0, |a| a.stage.chrome < 0.05),
        Step::Screenshot("s06-stage-chrome-faded"),
        Step::Key(egui::Key::Escape),
    ]);
    steps.extend(burst(&[
        "s07-stage-close-a",
        "s07-stage-close-b",
        "s07-stage-close-c",
        "s07-stage-close-d",
    ]));
    let home2 = View::Home.target();
    steps.extend([
        wait("stage closed", 5.0, |a| !a.stage.open && a.now_playing),
        Step::Sleep(1.0),
        click("Close player"),
        run("open Home", |a| a.open(View::Home)),
        wait("home", 30.0, move |a| {
            a.view == View::Home && loaded(a, &home2, 1)
        }),
        // Theme-painted covers, across a theme switch and back.
        click("Settings"),
        click("Paint covers"),
        wait("painting on", 10.0, |a| a.paint_covers),
        Step::Key(egui::Key::Escape),
        Step::Sleep(4.0),
        Step::Screenshot("s08-painted-home"),
        measure(
            "painted_theme",
            |a| json!({"dark": a.palette.dark, "window": format!("{:?}", a.palette.window)}),
        ),
        run("switch to a light theme", move |_| set_theme(&light)),
        wait("light colours", 120.0, |a| !a.palette.dark),
        Step::Sleep(4.0),
        Step::Screenshot("s09-painted-light"),
        measure(
            "painted_light_theme",
            |a| json!({"dark": a.palette.dark, "window": format!("{:?}", a.palette.window)}),
        ),
        run("switch the theme back", move |_| set_theme(&original)),
        wait("dark colours", 120.0, |a| a.palette.dark),
        Step::Sleep(4.0),
        Step::Screenshot("s10-painted-back"),
        click("Settings"),
        click("Paint covers"),
        wait("painting off", 10.0, |a| !a.paint_covers),
        Step::Key(egui::Key::Escape),
        Step::Sleep(2.0),
        Step::Screenshot("s11-unpainted-home"),
        run("restore the recent searches", move |a| {
            a.recent_searches = saved_2.borrow().clone();
            a.backend
                .send(Command::SaveSearches(a.recent_searches.clone()));
        }),
        run("pause", |a| {
            if a.playback.playing {
                a.backend.send(Command::TogglePause);
            }
        }),
    ]);
    steps
}

// ---- deck: Audition and Smooth mixes ----

/// What the backend read back from every deck, five times a second.
fn deck_samples() -> Vec<Value> {
    match probed("decks") {
        Value::Array(list) => list,
        _ => Vec::new(),
    }
}

/// Notes the run's clock under `key`, to line deck samples up with steps.
fn clock_mark(key: &'static str) -> Step {
    run(key, move |_| probe(key, json!(clock_ms())))
}

fn clock_at(key: &str) -> Option<u64> {
    probed(key).as_u64()
}

fn sample_ms(sample: &Value) -> u64 {
    sample["ms"].as_u64().unwrap_or(0)
}

/// A deck's amplitude as a share of the user's volume (mpv's volume
/// property is cubic in amplitude).
fn amplitude(sample: &Value, role: &str) -> Option<f64> {
    let volume = sample["volume"].as_f64().filter(|v| *v > 0.0)?;
    Some((sample[role]["volume"].as_f64()? / volume).powi(3))
}

fn rising(values: &[f64], slack: f64) -> bool {
    values.windows(2).all(|w| w[1] >= w[0] - slack)
}

fn queue_state(app: &App) -> Value {
    json!({"queue": queue_ids(app), "index": app.playback.index})
}

/// A song on screen, prepared ahead, other than the one playing.
fn audition_candidate(app: &App) -> Option<&crate::model::Item> {
    let playing = noted("deck:main", "id");
    page_items(app)
        .into_iter()
        .skip(1)
        .take(6)
        .find(|i| Some(item_id(i)) != playing && app.backend.prepared(&item_id(i)))
}

/// The audition, read back from the decks: the main deck's volume before,
/// during and after, both songs' positions, and the queue.
fn audition_report() -> Value {
    let samples = deck_samples();
    let (Some(hold), Some(release)) = (clock_at("deck:hold"), clock_at("deck:release")) else {
        return json!({"ok": false, "why": "the hold was not marked"});
    };
    let held = noted("deck:held", "id");
    let playing_at = samples
        .iter()
        .find(|s| {
            sample_ms(s) >= hold
                && s["held"]["playing"] == json!(true)
                && s["held"]["id"].as_str() == held.as_deref()
        })
        .map(sample_ms);
    let between = |from: u64, to: u64| {
        samples
            .iter()
            .filter(move |s| (from..to).contains(&sample_ms(s)))
    };
    let main_volumes = |from: u64, to: u64| -> Vec<f64> {
        between(from, to)
            .filter_map(|s| s["main"]["volume"].as_f64())
            .collect()
    };
    let volume = samples
        .iter()
        .rev()
        .find_map(|s| s["volume"].as_f64())
        .unwrap_or(0.0);
    let ducked = volume * 0.2f64.cbrt();
    let before = main_volumes(hold.saturating_sub(3000), hold);
    let during = playing_at.map_or_else(Vec::new, |p| main_volumes(p + 500, release));
    let after = main_volumes(release + 700, release + 2500);
    let near = |values: &[f64], target: f64| {
        !values.is_empty() && values.iter().all(|v| (v - target).abs() <= 1.5)
    };
    let audition_positions: Vec<f64> = playing_at.map_or_else(Vec::new, |p| {
        between(p, release)
            .filter_map(|s| s["audition"]["time_pos"].as_f64())
            .collect()
    });
    let main: Vec<(u64, f64, u64)> = between(hold.saturating_sub(2000), release + 2500)
        .filter_map(|s| {
            Some((
                sample_ms(s),
                s["main"]["time_pos"].as_f64()?,
                s["main"]["serial"].as_u64()?,
            ))
        })
        .collect();
    let main_continued = main.len() >= 10
        && main
            .windows(2)
            .all(|w| w[1].1 >= w[0].1 && w[1].2 == w[0].2)
        && main
            .first()
            .zip(main.last())
            .is_some_and(|(a, b)| b.1 - a.1 >= 0.8 * (b.0 - a.0) as f64 / 1000.0);
    let audition_advanced = audition_positions.len() >= 5
        && rising(&audition_positions, 0.0)
        && audition_positions.last().unwrap_or(&0.0) - audition_positions.first().unwrap_or(&0.0)
            >= 1.0;
    let queue_unchanged = !probed("deck:queue_before").is_null()
        && probed("deck:queue_before") == probed("deck:queue_after");
    let ok = near(&before, volume)
        && near(&during, ducked)
        && near(&after, volume)
        && audition_advanced
        && main_continued
        && queue_unchanged;
    json!({
        "ok": ok,
        "volume": volume,
        "ducked_volume_expected": ducked,
        "main_volume_before": before,
        "main_volume_during": during,
        "main_volume_after": after,
        "audition_started_after_hold_ms": playing_at.map(|p| p.saturating_sub(hold)),
        "audition_positions": audition_positions,
        "audition_advanced": audition_advanced,
        "main_positions": main.iter().map(|m| m.1).collect::<Vec<_>>(),
        "main_continued": main_continued,
        "queue_unchanged": queue_unchanged,
    })
}

/// The blend into the radio's next song, read back from both decks.
fn blend_report(app: &App) -> Value {
    let samples = deck_samples();
    let Some(seek) = clock_at("deck:blend_seek") else {
        return json!({"ok": false, "why": "the seek was not marked"});
    };
    let window: Vec<&Value> = samples.iter().filter(|s| sample_ms(s) >= seek).collect();
    let blend: Vec<&Value> = window
        .iter()
        .copied()
        .filter(|s| !s["tail"].is_null())
        .collect();
    let pairs: Vec<(f64, f64)> = blend
        .iter()
        .filter_map(|s| amplitude(s, "main").zip(amplitude(s, "tail")))
        .collect();
    let rows: Vec<Value> = blend
        .iter()
        .map(|s| {
            let (i, o) = (amplitude(s, "main"), amplitude(s, "tail"));
            json!({
                "ms": sample_ms(s) - seek,
                "in_volume": s["main"]["volume"],
                "out_volume": s["tail"]["volume"],
                "in_amplitude": i,
                "out_amplitude": o,
                "power": i.zip(o).map(|(i, o)| i * i + o * o),
                "progress": s["blend"],
            })
        })
        .collect();
    let ins: Vec<f64> = pairs.iter().map(|p| p.0).collect();
    let outs: Vec<f64> = pairs.iter().map(|p| -p.1).collect();
    let equal_power = pairs.len() >= 10
        && pairs
            .iter()
            .all(|(i, o)| (0.85..=1.15).contains(&(i * i + o * o)));
    let crossfaded = rising(&ins, 0.03)
        && rising(&outs, 0.03)
        && ins.first().is_some_and(|i| *i < 0.4)
        && outs.last().is_some_and(|o| -o < 0.4);
    let serial = |s: &Value| s["main"]["serial"].as_u64();
    let before_serial = samples
        .iter()
        .rev()
        .find(|s| sample_ms(s) < seek)
        .and_then(serial);
    let blend_end = blend.last().map(|s| sample_ms(s));
    let after: Vec<&Value> = blend_end.map_or_else(Vec::new, |end| {
        window
            .iter()
            .copied()
            .filter(|s| (end + 400..end + 3000).contains(&sample_ms(s)))
            .collect()
    });
    let after_full = !after.is_empty()
        && after
            .iter()
            .all(|s| s["tail"].is_null() && amplitude(s, "main").is_some_and(|a| a > 0.95));
    let two_decks =
        before_serial.is_some() && after.first().and_then(|s| serial(s)) != before_serial;
    let next_current = current_id(app) == json!(noted("deck:radio", "next"));
    let ok = equal_power && crossfaded && after_full && two_decks && next_current;
    json!({
        "ok": ok,
        "equal_power": equal_power,
        "crossfaded": crossfaded,
        "full_volume_after": after_full,
        "decks_swapped": two_decks,
        "next_song_current": next_current,
        "length_s": blend.first().zip(blend.last()).map(|(a, b)| (sample_ms(b) - sample_ms(a)) as f64 / 1000.0),
        "samples": rows,
    })
}

/// The album's change between its first two songs: the second one queued
/// behind the first in the same mpv, and no blend.
fn album_report(app: &App) -> Value {
    let Some(seek) = clock_at("deck:album_seek") else {
        return json!({"ok": false, "why": "the seek was not marked"});
    };
    let samples = deck_samples();
    let window: Vec<&Value> = samples.iter().filter(|s| sample_ms(s) >= seek).collect();
    let blended = window.iter().any(|s| !s["tail"].is_null());
    let serials: std::collections::BTreeSet<u64> = window
        .iter()
        .filter_map(|s| s["main"]["serial"].as_u64())
        .collect();
    let last_first = window.iter().rev().find(|s| s["index"] == json!(0));
    let first_second = window.iter().find(|s| s["index"] == json!(1));
    // Wall time between the two samples against the audio between them.
    let gap = last_first.zip(first_second).and_then(|(a, b)| {
        let left = a["duration"].as_f64()? - a["position"].as_f64()?;
        let into = b["position"].as_f64()?;
        Some((sample_ms(b) - sample_ms(a)) as f64 / 1000.0 - left - into)
    });
    let queued = window
        .first()
        .is_some_and(|s| s["next_ready"] == json!(true) && s["cued"].is_null());
    let changed = first_second.is_some() && app.playback.index == Some(1);
    let ok = !blended && serials.len() == 1 && queued && changed;
    json!({
        "ok": ok,
        "blended": blended,
        "one_deck": serials.len() == 1,
        "next_queued_behind_in_mpv": queued,
        "changed_to_second_song": changed,
        "gap_estimate_s": gap,
    })
}

/// Audition and Smooth mixes. A playlist plays; a song on screen is held
/// with Alt under the pointer and auditioned over the ducked current song,
/// then let go. Smooth mixes go on in Settings; a radio blends into its next
/// song with an equal-power crossfade on two decks; an album's songs still
/// change gaplessly on one. Puts the volume, repeat, shuffle and the setting back.
fn deck() -> Vec<Step> {
    let alt = egui::Modifiers {
        alt: true,
        ..Default::default()
    };
    vec![
        wait("signed in", 60.0, |a| {
            matches!(a.account, Account::SignedIn { .. })
        }),
        run(
            "note what to put back; volume 60, repeat off, unshuffled, no mixes; sample the decks",
            |a| {
                probe(
                    "deck:original",
                    json!({
                        "volume": a.playback.volume,
                        "repeat": a.playback.repeat,
                        "shuffle": a.playback.shuffle,
                        "mixes": a.playback.mixes,
                    }),
                );
                let cycles = match a.playback.repeat {
                    crate::model::Repeat::Off => 0,
                    crate::model::Repeat::All => 2,
                    crate::model::Repeat::One => 1,
                };
                for _ in 0..cycles {
                    a.backend.send(Command::CycleRepeat);
                }
                if a.playback.shuffle {
                    a.backend.send(Command::ToggleShuffle);
                }
                a.backend.send(Command::Volume(60.0));
                a.backend.send(Command::Mixes(crate::model::Mixes {
                    on: false,
                    ..a.playback.mixes
                }));
                a.backend.send(Command::SampleDecks(true));
            },
        ),
        wait("library playlists", 60.0, |a| library_playlist(a).is_some()),
        click_with("a library playlist with 8+ songs", library_playlist),
        wait("playlist page", 60.0, current_loaded),
        wait("songs on the page", 30.0, |a| page_items(a).len() >= 6),
        run("pick the song to play", |a| {
            if let Some(item) = page_items(a).first() {
                note_item("deck:main", item);
            }
        }),
        click_with("the first song's cover", |_| {
            noted("deck:main", "title").map(|t| format!("Play {t}"))
        }),
        wait("the first song plays", 90.0, |a| {
            audible(a) && current_id(a) == json!(noted("deck:main", "id"))
        }),
        // Audition: Alt held with the pointer resting on another song.
        wait("a song on screen prepared", 120.0, |a| {
            audition_candidate(a).is_some()
        }),
        run("pick it to audition", |a| {
            if let Some(item) = audition_candidate(a) {
                note_item("deck:held", item);
            }
        }),
        run("note the queue", |a| {
            probe("deck:queue_before", queue_state(a))
        }),
        hover_with("the song to audition", |_| noted("deck:held", "title")),
        Step::Sleep(3.0),
        clock_mark("deck:hold"),
        Step::Hold(alt),
        wait("the audition plays", 30.0, |a| {
            a.playback.audition.as_ref().is_some_and(|x| {
                x.playing && Some(&x.video_id) == noted("deck:held", "id").as_ref()
            })
        }),
        measure("audition_playing_after_hold_ms", |_| {
            json!(clock_at("deck:hold").map(|h| clock_ms().saturating_sub(h)))
        }),
        Step::Sleep(1.5),
        Step::Screenshot("d01-audition"),
        Step::Sleep(2.0),
        clock_mark("deck:release"),
        Step::Hold(egui::Modifiers::NONE),
        wait("the audition ends", 5.0, |a| a.playback.audition.is_none()),
        Step::Sleep(3.0),
        run("note the queue again", |a| {
            probe("deck:queue_after", queue_state(a))
        }),
        measure("audition", |_| audition_report()),
        wait(
            "audition: main ducked and back, both songs moved on, queue unchanged",
            1.0,
            |_| audition_report()["ok"] == json!(true),
        ),
        // Smooth mixes, turned on in Settings.
        click("Settings"),
        click("Blend songs on radios and mixes"),
        wait("Smooth mixes on", 5.0, |a| a.playback.mixes.on),
        Step::Screenshot("d02-smooth-mixes-setting"),
        Step::Key(egui::Key::Escape),
        run(
            "start a radio from the playing song, as Start radio does",
            |a| {
                if let Some(track) = a.playback.index.and_then(|i| a.queue.get(i)) {
                    let id = track.video_id.clone();
                    a.backend.send(Command::PlayTarget(Target::Watch {
                        video_id: Some(id.clone()),
                        playlist_id: Some(format!("RDAMVM{id}")),
                        params: Some("wAEB".into()),
                    }));
                }
            },
        ),
        wait("the radio plays", 90.0, |a| {
            audible(a)
                && a.queue.len() > 5
                && json!(queue_ids(a)) != probed("deck:queue_before")["queue"]
        }),
        wait("the next song cued on the second deck", 120.0, |a| {
            a.playback.next_ready && a.playback.duration > 40.0
        }),
        run("note the radio's songs", |a| {
            probe(
                "deck:radio",
                json!({
                    "current": current_id(a),
                    "next": upcoming(a, 1).first(),
                    "duration": a.playback.duration,
                    "seconds": a.playback.mixes.seconds,
                }),
            );
        }),
        clock_mark("deck:blend_seek"),
        run("seek to 10 s before the end", |a| {
            a.backend.send(Command::Seek(a.playback.duration - 10.0));
        }),
        wait("the next song is current", 30.0, |a| {
            current_id(a) == json!(noted("deck:radio", "next"))
        }),
        Step::Sleep(1.5),
        Step::Screenshot("d03-blend"),
        wait("the blend is over", 30.0, |_| {
            let samples = deck_samples();
            let seek = clock_at("deck:blend_seek").unwrap_or(u64::MAX);
            let window: Vec<&Value> = samples.iter().filter(|s| sample_ms(s) >= seek).collect();
            window.iter().any(|s| !s["tail"].is_null())
                && window.last().is_some_and(|s| s["tail"].is_null())
        }),
        Step::Sleep(3.0),
        measure("blend", blend_report),
        wait(
            "blend: equal power on two decks, then the next song alone",
            1.0,
            |a| blend_report(a)["ok"] == json!(true),
        ),
        // An album stays gapless with Smooth mixes on.
        run("open an album", |a| {
            a.open(View::Page(Target::browse(E2E_ALBUM)))
        }),
        wait("album page", 60.0, current_loaded),
        run("pick its first song", |a| {
            if let Some(item) = page_items(a).first() {
                note_item("deck:album", item);
            }
        }),
        click_with("the album's first song", |_| {
            noted("deck:album", "title").map(|t| format!("Play {t}"))
        }),
        wait("the album plays", 90.0, |a| {
            audible(a) && current_id(a) == json!(noted("deck:album", "id"))
        }),
        wait("the second song queued behind it", 120.0, |a| {
            a.playback.next_ready && a.playback.duration > 20.0
        }),
        clock_mark("deck:album_seek"),
        run("seek to 6 s before the end", |a| {
            a.backend.send(Command::Seek(a.playback.duration - 6.0));
        }),
        wait("the second song is current", 30.0, |a| {
            a.playback.index == Some(1) && audible(a)
        }),
        Step::Sleep(3.0),
        measure("album", album_report),
        wait("album: gapless on one deck, no blend", 1.0, |a| {
            album_report(a)["ok"] == json!(true)
        }),
        // Smooth mixes off again, in Settings.
        click("Settings"),
        click("Blend songs on radios and mixes"),
        wait("Smooth mixes off", 5.0, |a| !a.playback.mixes.on),
        Step::Key(egui::Key::Escape),
        run("pause and put back volume, repeat, shuffle", |a| {
            let original = probed("deck:original");
            if a.playback.playing {
                a.backend.send(Command::TogglePause);
            }
            if let Some(volume) = original["volume"].as_f64() {
                a.backend.send(Command::Volume(volume));
            }
            let cycles = match original["repeat"].as_str() {
                Some("All") => 1,
                Some("One") => 2,
                _ => 0,
            };
            for _ in 0..cycles {
                a.backend.send(Command::CycleRepeat);
            }
            if original["shuffle"] == json!(true) {
                a.backend.send(Command::ToggleShuffle);
            }
            if let Ok(mixes) = serde_json::from_value(original["mixes"].clone()) {
                a.backend.send(Command::Mixes(mixes));
            }
            a.backend.send(Command::SampleDecks(false));
        }),
        wait("settings put back", 10.0, |a| {
            let original = probed("deck:original");
            original["volume"].as_f64() == Some(a.playback.volume)
                && original["mixes"] == json!(a.playback.mixes)
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
    /// The controls named the frame before `frame`'s registry.
    previous: Vec<(String, Rect)>,
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
            previous: Vec::new(),
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
            // The run is over: quit, even with music in the queue.
            app.quit(ctx);
            return;
        }
        // Only controls drawn in the same place for two frames are pointed
        // at: a popup or dialog lays itself out once off its final place
        // (egui's sizing pass) before it shows, and a moving control would
        // be missed.
        let stable: Vec<(String, Rect)> = registry
            .iter()
            .filter(|(label, rect)| {
                self.previous.iter().any(|(l, r)| {
                    l == label
                        && (r.min - rect.min).length() < 1.0
                        && (r.max - rect.max).length() < 1.0
                })
            })
            .cloned()
            .collect();
        self.previous = registry;
        // The steps are taken out while one runs, so it can log and advance.
        let steps = std::mem::take(&mut self.steps);
        self.step(&steps[self.index], app, ctx, &stable);
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
                    let found = wanted.iter().find_map(|w| {
                        registry
                            .iter()
                            .rev()
                            .find(|(l, r)| l == w && screen.contains(r.center()))
                            .map(|(l, r)| (l.clone(), *r))
                    });
                    match found {
                        Some((name, rect)) => {
                            self.note(&format!("click {describe} = {name:?}"));
                            self.pending.push(Event::PointerMoved(rect.center()));
                            self.phase = Phase::Move(rect.center());
                        }
                        None if elapsed > *timeout => {
                            let shown: Vec<&String> = wanted.iter().take(3).collect();
                            let message =
                                format!("no visible control named {describe} ({shown:?})");
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
            Step::Paste(text) => match text(app) {
                Some(text) => {
                    self.note(&format!("paste {text:?}"));
                    self.pending.push(Event::Paste(text));
                    self.advance();
                }
                None => self.fail("nothing to paste".into()),
            },
            Step::Window(what, command) => {
                ctx.send_viewport_cmd(command.clone());
                self.note(&format!("window: {what}"));
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
            Step::Hold(modifiers) => {
                self.pending.push(Event::ModifiersChanged(*modifiers));
                self.note(&format!("hold {modifiers:?}"));
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
            Step::Leave => {
                self.pending.push(Event::PointerGone);
                self.note("pointer leaves");
                self.advance();
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
