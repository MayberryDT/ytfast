//! Short videos of what Music does, for showing people (`scripts/demo.sh`).
//! Each `demo-*` scenario runs signed out on public YouTube Music: it gets
//! ready off camera, then a "roll" cue starts the stretch kept on video, with
//! the pointer gliding between controls and the keys pressed shown; "cut"
//! ends it. Waits that would only show a still screen sit between a cut and
//! the next roll. Anything a clip changes is put back after its last cut.

use egui::{Key, Modifiers, ViewportCommand};

use super::{
    Step, audible, click, click_with, current_loaded, drag_with, fact, hover_with, loaded,
    omarchy_theme, open_item, page_items, right_click_first_visible, run, searched, set_fact,
    set_theme, song_on_page, timed_lines, wait,
};
use crate::app::{App, NowPlayingTab, View};
use crate::backend::Command;
use crate::model::{Account, ItemKind, Target};

pub(super) fn scenario(name: &str) -> Vec<Step> {
    match name {
        "demo-stage" => stage(),
        "demo-themes" => themes(),
        "demo-audition" => audition(),
        "demo-mix" => mix(),
        "demo-keys" => keys(),
        "demo-sound" => sound(),
        _ => flight(),
    }
}

/// The song most clips play: it has timed lyrics and a most-replayed part.
const SONG: &str = "Get Lucky";

fn cue(name: &'static str) -> Step {
    Step::Cue(name)
}

/// Text typed a key at a time, as a person types.
fn typed(text: &str) -> Vec<Step> {
    text.chars()
        .flat_map(|c| [Step::Type(c.to_string()), Step::Sleep(0.07)])
        .collect()
}

/// Signed out, Home loaded, the window filling the screen, volume up.
fn ready() -> Vec<Step> {
    let home = View::Home.target();
    vec![
        wait("signed out", 60.0, |a| {
            matches!(a.account, Account::SignedOut { .. })
        }),
        wait("public home", 60.0, move |a| loaded(a, &home, 1)),
        run("volume up", |a| a.backend.send(Command::Volume(100.0))),
        Step::Window("fill the screen", ViewportCommand::Fullscreen(true)),
        Step::Sleep(3.0),
    ]
}

/// Searches for `query` off camera and waits until `SONG` on the results is
/// ready to play at once.
fn find_song(query: &'static str) -> Vec<Step> {
    let mut steps = vec![
        run("clear the search field", |a| a.search.clear()),
        click("Search"),
    ];
    steps.extend(typed(query));
    steps.extend([
        Step::Key(Key::Enter),
        wait("search results", 60.0, searched),
        wait("the song ready to play", 90.0, |a| {
            song_on_page(a, SONG).is_some_and(|t| a.backend.prepared(&t.video_id))
        }),
    ]);
    steps
}

fn play_song() -> Step {
    click_with("the song's cover", |a| {
        song_on_page(a, SONG).map(|t| format!("Play {}", t.title))
    })
}

fn playing(a: &App) -> bool {
    a.playback.playing && a.playback.position > 0.5 && a.playback.duration > 0.0
}

fn pause() -> Step {
    run("pause", |a| {
        if a.playback.playing {
            a.backend.send(Command::TogglePause);
        }
    })
}

/// Covers fly: Home's cards lift under the pointer, a search, the song's
/// cover flies into Now Playing, the jump to the most replayed part, timed
/// lyrics.
fn flight() -> Vec<Step> {
    let home = View::Home.target();
    let mut steps = ready();
    // Off camera: the search once, so the song plays the moment it's clicked.
    steps.extend(find_song(SONG));
    steps.extend([
        run("back to Home", |a| {
            a.search.clear();
            a.open(View::Home);
        }),
        wait("home", 30.0, move |a| {
            a.view == View::Home && loaded(a, &home, 1)
        }),
        Step::Sleep(2.5),
        cue("roll"),
        Step::Sleep(0.8),
    ]);
    for n in [0, 1, 2] {
        steps.extend([
            hover_with("a card on Home", move |a| home_card(a, n)),
            Step::Sleep(0.45),
        ]);
    }
    steps.push(click("Search"));
    steps.extend(typed(SONG));
    steps.extend([
        Step::Sleep(0.3),
        Step::Key(Key::Enter),
        wait("search results", 60.0, searched),
        Step::Sleep(1.2),
        play_song(),
        wait("playing", 30.0, playing),
        Step::Sleep(1.8),
        wait("now playing", 10.0, |a| a.now_playing),
        Step::Sleep(1.8),
        click("LYRICS"),
        wait("timed lyrics", 30.0, |a| timed_lines(a).is_some()),
        Step::Sleep(1.2),
        click("Jump to the most replayed part"),
        Step::Sleep(5.5),
        cue("cut"),
        pause(),
    ]);
    steps
}

/// The `n`th card in Home's first shelf.
fn home_card(app: &App, n: usize) -> Option<String> {
    let page = app.page_state(&View::Home.target())?.page.as_ref()?;
    let shelf = page.shelves.iter().find(|s| s.items.len() > n)?;
    shelf.items.get(n).map(|i| i.title.clone())
}

/// Most replayed and Stage: the ridge rises under the pointer, the cover
/// flies into Stage, big timed lyrics with the chrome fading away, and back.
fn stage() -> Vec<Step> {
    let mut steps = ready();
    steps.extend(find_song(SONG));
    steps.extend([
        play_song(),
        wait("playing", 30.0, playing),
        wait("most replayed known", 30.0, |a| {
            a.current_heat().and_then(|h| h.peak).is_some()
        }),
        run("a little before the most replayed part", |a| {
            if let Some(peak) = a.current_heat().and_then(|h| h.peak) {
                a.backend.send(Command::Seek((peak.start - 12.0).max(0.0)));
            }
        }),
        wait("now playing", 10.0, |a| a.now_playing),
        click("LYRICS"),
        wait("timed lyrics", 30.0, |a| timed_lines(a).is_some()),
        Step::Leave,
        Step::Sleep(2.0),
        cue("roll"),
        Step::Sleep(0.8),
        hover_with("the seek bar", |_| Some("Seek".into())),
        Step::Sleep(2.2),
        Step::Leave,
        Step::Sleep(0.4),
        Step::Key(Key::F),
        wait("stage open", 5.0, |a| a.stage.open),
        Step::Sleep(8.0),
        Step::Key(Key::Escape),
        wait("stage closed", 5.0, |a| !a.stage.open),
        Step::Sleep(1.5),
        cue("cut"),
        pause(),
    ]);
    steps
}

/// Omarchy themes, live: every colour follows the desktop's theme while the
/// cover keeps its own; then covers painted in the theme. Off camera between
/// switches while `omarchy-theme-set` works; puts the theme back.
fn themes() -> Vec<Step> {
    let original = omarchy_theme().unwrap_or_else(|| "Permafrost".into());
    let others: Vec<String> = ["Stakeout", "Snow", "Study", "Permafrost"]
        .into_iter()
        .filter(|t| *t != original)
        .map(String::from)
        .take(3)
        .collect();
    let mut steps = ready();
    steps.extend(find_song(SONG));
    steps.extend([
        play_song(),
        wait("playing", 30.0, playing),
        wait("now playing", 10.0, |a| a.now_playing),
        Step::Leave,
        Step::Sleep(2.0),
        cue("roll"),
        Step::Sleep(2.0),
    ]);
    for theme in others.iter().cloned() {
        steps.extend(switch_theme(theme));
    }
    // Covers painted in the theme, on Home.
    steps.extend([
        click("Close player"),
        run("open Home", |a| a.open(View::Home)),
        Step::Sleep(1.2),
        click("Settings"),
        Step::Sleep(0.6),
        click("Paint covers"),
        wait("painting on", 10.0, |a| a.paint_covers),
        Step::Sleep(0.5),
        Step::Key(Key::Escape),
        Step::Leave,
        Step::Sleep(3.0),
    ]);
    steps.extend(switch_theme(original.clone()));
    steps.extend([
        cue("cut"),
        click("Settings"),
        click("Paint covers"),
        wait("painting off", 10.0, |a| !a.paint_covers),
        Step::Key(Key::Escape),
        pause(),
    ]);
    steps
}

/// Switches the desktop theme with the camera off until the colours start
/// to move, then watches them settle.
fn switch_theme(theme: String) -> Vec<Step> {
    vec![
        cue("cut"),
        run("note the colours", |a| {
            set_fact("demo_window", format!("{:?}", a.palette.window));
        }),
        run("switch the desktop theme", move |_| set_theme(&theme)),
        wait("the colours start to change", 120.0, |a| {
            fact("demo_window") != Some(format!("{:?}", a.palette.window))
        }),
        cue("roll"),
        Step::Sleep(3.0),
    ]
}

/// Songs on the page other than the playing one, ready to play.
fn ready_songs(app: &App) -> Vec<String> {
    let playing = app.current_track().map(|t| t.video_id.clone());
    page_items(app)
        .into_iter()
        .filter(|i| i.kind == ItemKind::Song)
        .filter_map(|i| i.track.as_ref())
        .filter(|t| Some(&t.video_id) != playing.as_ref() && app.backend.prepared(&t.video_id))
        .map(|t| t.title.clone())
        .collect()
}

/// Audition: Alt held over another song plays it over the ducked current
/// one; letting go brings the current song back. Twice.
fn audition() -> Vec<Step> {
    let alt = Modifiers {
        alt: true,
        ..Default::default()
    };
    let mut steps = ready();
    steps.extend([
        run("clear the search field", |a| a.search.clear()),
        click("Search"),
    ]);
    steps.extend(typed("Daft Punk"));
    steps.extend([
        Step::Key(Key::Enter),
        wait("search results", 60.0, searched),
        run("open the artist", |a| {
            open_item(a, |i| i.kind == ItemKind::Artist);
        }),
        wait("artist page", 60.0, current_loaded),
        click_with("the first top song", |a| {
            page_items(a)
                .into_iter()
                .find(|i| i.kind == ItemKind::Song)
                .map(|i| format!("Play {}", i.title))
        }),
        wait("playing", 90.0, audible),
        click("Close player"),
        wait("two more songs ready", 120.0, |a| ready_songs(a).len() >= 2),
        run("pick them", |a| {
            let songs = ready_songs(a);
            set_fact("demo_held_1", songs[0].clone());
            set_fact("demo_held_2", songs[1].clone());
        }),
        Step::Leave,
        Step::Sleep(2.0),
        cue("roll"),
        Step::Sleep(1.0),
    ]);
    for key in ["demo_held_1", "demo_held_2"] {
        steps.extend([
            hover_with("a song to audition", move |_| fact(key)),
            Step::Sleep(0.7),
            Step::Hold(alt),
            wait("the audition plays", 30.0, |a| {
                a.playback.audition.as_ref().is_some_and(|x| x.playing)
            }),
            Step::Sleep(4.0),
            Step::Hold(Modifiers::NONE),
            wait("the audition ends", 5.0, |a| a.playback.audition.is_none()),
            Step::Sleep(2.0),
        ]);
    }
    steps.extend([cue("cut"), pause()]);
    steps
}

/// Smooth mixes: on a radio, one song blends into the next.
fn mix() -> Vec<Step> {
    let mut steps = ready();
    steps.extend([
        click("Settings"),
        click("Blend songs on radios and mixes"),
        wait("Smooth mixes on", 5.0, |a| a.playback.mixes.on),
        Step::Key(Key::Escape),
    ]);
    steps.extend(find_song(SONG));
    steps.extend([
        play_song(),
        wait("playing", 30.0, playing),
        run("start a radio from the song, as Start radio does", |a| {
            if let Some(track) = a.current_track() {
                let id = track.video_id.clone();
                a.backend.send(Command::PlayTarget(Target::Watch {
                    video_id: Some(id.clone()),
                    playlist_id: Some(format!("RDAMVM{id}")),
                    params: Some("wAEB".into()),
                }));
            }
        }),
        wait("the radio plays", 90.0, |a| audible(a) && a.queue.len() > 5),
        wait("now playing", 10.0, |a| a.now_playing),
        wait("the next song cued on the second deck", 120.0, |a| {
            a.playback.next_ready && a.playback.duration > 40.0
        }),
        run("note the song", |a| {
            set_fact(
                "demo_mix_from",
                a.current_track()
                    .map(|t| t.video_id.clone())
                    .unwrap_or_default(),
            );
        }),
        run("near the end", |a| {
            a.backend.send(Command::Seek(a.playback.duration - 19.0));
        }),
        Step::Leave,
        Step::Sleep(1.5),
        cue("roll"),
        Step::Sleep(0.8),
        click("Settings"),
        hover_with("the Smooth mixes setting", |_| {
            Some("Blend songs on radios and mixes".into())
        }),
        Step::Sleep(1.8),
        Step::Key(Key::Escape),
        Step::Leave,
        wait("the next song is current", 30.0, |a| {
            a.current_track().map(|t| t.video_id.clone()) != fact("demo_mix_from")
        }),
        Step::Sleep(7.0),
        cue("cut"),
        click("Settings"),
        click("Blend songs on radios and mixes"),
        wait("Smooth mixes off", 5.0, |a| !a.playback.mixes.on),
        Step::Key(Key::Escape),
        pause(),
    ]);
    steps
}

/// The keyboard: Play anything (Ctrl+K), the shortcuts (`?`), a song's menu
/// (Play next, Add to queue) and Up next, reordered by dragging.
fn keys() -> Vec<Step> {
    let mut steps = ready();
    steps.extend([
        run("clear the search field", |a| a.search.clear()),
        click("Search"),
    ]);
    steps.extend(typed("Daft Punk"));
    steps.extend([
        Step::Key(Key::Enter),
        wait("search results", 60.0, searched),
        run("open the artist", |a| {
            open_item(a, |i| i.kind == ItemKind::Artist);
        }),
        wait("artist page", 60.0, current_loaded),
        click_with("the first top song", |a| {
            page_items(a)
                .into_iter()
                .find(|i| i.kind == ItemKind::Song)
                .map(|i| format!("Play {}", i.title))
        }),
        wait("playing", 90.0, audible),
        click("Close player"),
        run("pick songs for the menu", |a| {
            let songs: Vec<String> = page_items(a)
                .into_iter()
                .filter(|i| i.kind == ItemKind::Song)
                .map(|i| i.title.clone())
                .collect();
            if let [_, _, next, _, queued, ..] = songs.as_slice() {
                set_fact("demo_next", next.clone());
                set_fact("demo_queued", queued.clone());
            }
        }),
        wait("songs picked", 1.0, |_| fact("demo_queued").is_some()),
        Step::Leave,
        Step::Sleep(2.0),
        cue("roll"),
        Step::Sleep(0.8),
        Step::KeyWith(Modifiers::CTRL, Key::K),
        wait("Play anything open", 2.0, |a| a.control.play_anything.open),
        Step::Sleep(0.4),
    ]);
    steps.extend(typed("digital love"));
    steps.extend([
        wait("results", 15.0, |a| {
            let pa = &a.control.play_anything;
            !pa.searching() && pa.hits().len() >= 3
        }),
        Step::Sleep(1.0),
        run("note the song", |a| {
            set_fact(
                "demo_before",
                a.current_track()
                    .map(|t| t.video_id.clone())
                    .unwrap_or_default(),
            );
        }),
        Step::Key(Key::Enter),
        wait("the song plays", 30.0, |a| {
            audible(a) && a.current_track().map(|t| t.video_id.clone()) != fact("demo_before")
        }),
        Step::Sleep(1.5),
        Step::KeyWith(Modifiers::SHIFT, Key::Questionmark),
        wait("shortcuts", 2.0, |a| a.control.help),
        Step::Sleep(2.8),
        Step::Key(Key::Escape),
        Step::Sleep(0.6),
        right_click_first_visible("a song for Play next", |_| {
            fact("demo_next")
                .map(|t| format!("Play {t}"))
                .into_iter()
                .collect()
        }),
        Step::Sleep(0.7),
        click("Play next"),
        Step::Sleep(0.4),
        right_click_first_visible("a song for Add to queue", |_| {
            fact("demo_queued")
                .map(|t| format!("Play {t}"))
                .into_iter()
                .collect()
        }),
        Step::Sleep(0.7),
        click("Add to queue"),
        Step::Sleep(0.4),
        Step::Key(Key::Q),
        wait("Up next", 3.0, |a| {
            a.now_playing && a.now_playing_tab == NowPlayingTab::UpNext
        }),
        Step::Sleep(1.5),
        drag_with(
            "the Play next song below the added one",
            |_| fact("demo_next").map(|t| format!("Reorder {t}")),
            |_| fact("demo_queued"),
        ),
        Step::Sleep(2.0),
        cue("cut"),
        pause(),
    ]);
    steps
}

/// The equalizer's presets on the playing song, then the sleep timer.
fn sound() -> Vec<Step> {
    let mut steps = ready();
    steps.extend(find_song(SONG));
    steps.extend([
        play_song(),
        wait("playing", 30.0, playing),
        run("into the song", |a| a.backend.send(Command::Seek(50.0))),
        Step::Leave,
        Step::Sleep(2.0),
        cue("roll"),
        Step::Sleep(0.8),
        click("Settings"),
        Step::Sleep(0.4),
        click("Open equalizer"),
        wait("equalizer open", 5.0, |a| a.equalizer_open),
        Step::Sleep(1.2),
    ]);
    for preset in ["Bass boost", "Late night", "Rock"] {
        steps.extend([click(preset), Step::Sleep(2.6)]);
    }
    steps.extend([
        click("Close equalizer"),
        wait("equalizer closed", 5.0, |a| !a.equalizer_open),
        wait("now playing", 10.0, |a| a.now_playing),
        Step::Sleep(0.6),
        click("Sleep timer"),
        Step::Sleep(1.0),
        click("End of song"),
        Step::Sleep(2.0),
        cue("cut"),
        run("timer off, flat, pause", |a| {
            a.backend.send(Command::SleepTimer(None));
            if a.playback.playing {
                a.backend.send(Command::TogglePause);
            }
        }),
        click("Settings"),
        click("Open equalizer"),
        click("Flat"),
        click("Close equalizer"),
        Step::Sleep(1.0),
    ]);
    steps
}
