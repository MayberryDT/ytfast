//! The keyboard map (docs/SPEC.md § Control). One table drives both the
//! shortcuts and the `?` overlay that lists them, including the keys other
//! parts of the app answer (Stage, the most-replayed jump, Audition).
//! Nothing fires while a text field has focus, except Esc and Ctrl+K.

use super::motion;
use super::widgets::{font, label, named};
use crate::app::{Action, App, NowPlayingTab, WindowKind};
use crate::backend::Command;
use crate::control::ControlAction;
use crate::theme::Palette;
use egui::{
    Align2, Context, CornerRadius, Event, Id, Key, LayerId, Order, Rect, Sense, Stroke, Ui, Vec2,
    emath::TSTransform, pos2, vec2,
};
use fastframe_fonts::Weight;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Playback,
    Library,
    Navigation,
    Views,
}

impl Group {
    fn title(self) -> &'static str {
        match self {
            Group::Playback => "Playback",
            Group::Library => "Library",
            Group::Navigation => "Navigation",
            Group::Views => "Views",
        }
    }
}

/// What a shortcut does.
#[derive(Clone, Copy, PartialEq)]
enum Do {
    TogglePause,
    /// Seek by this many seconds.
    Seek(f64),
    Previous,
    Next,
    /// Turn the volume by this many points.
    Volume(f64),
    Mute,
    Shuffle,
    Repeat,
    Like,
    Search,
    PlayAnything,
    Back,
    Forward,
    /// Close the topmost menu, palette, overlay or Now Playing.
    Close,
    NowPlaying,
    UpNext,
    Equalizer,
    Settings,
    Mini,
    Help,
    Quit,
}

impl Do {
    /// Held keys repeat these; everything else answers the first press only.
    fn repeats(self) -> bool {
        matches!(self, Do::Seek(_) | Do::Volume(_))
    }
}

/// A key with its modifiers. `shift: None` takes the key with or without
/// Shift (`?` and `+` need it on most layouts).
#[derive(Clone, Copy)]
struct Chord {
    key: Key,
    ctrl: bool,
    alt: bool,
    shift: Option<bool>,
}

const fn plain(key: Key) -> Chord {
    Chord {
        key,
        ctrl: false,
        alt: false,
        shift: Some(false),
    }
}

const fn either_shift(key: Key) -> Chord {
    Chord {
        shift: None,
        ..plain(key)
    }
}

const fn shift(key: Key) -> Chord {
    Chord {
        shift: Some(true),
        ..plain(key)
    }
}

const fn ctrl(key: Key) -> Chord {
    Chord {
        ctrl: true,
        ..plain(key)
    }
}

const fn alt(key: Key) -> Chord {
    Chord {
        alt: true,
        ..plain(key)
    }
}

impl Chord {
    fn matches(&self, key: Key, m: egui::Modifiers) -> bool {
        self.key == key
            && (m.ctrl || m.command) == self.ctrl
            && m.alt == self.alt
            && self.shift.is_none_or(|s| s == m.shift)
    }
}

/// One line of the overlay: the keys as shown (alternatives, each a
/// combination of caps), what they do, and the chords that do it. Lines
/// with no chords are answered elsewhere and only listed here.
struct Shortcut {
    group: Group,
    keys: &'static [&'static [&'static str]],
    what: &'static str,
    chords: &'static [(Chord, Do)],
}

const MAP: &[Shortcut] = &[
    Shortcut {
        group: Group::Playback,
        keys: &[&["Space"]],
        what: "Play or pause",
        chords: &[(plain(Key::Space), Do::TogglePause)],
    },
    Shortcut {
        group: Group::Playback,
        keys: &[&["←"], &["→"]],
        what: "Back or forward 5 seconds",
        chords: &[
            (plain(Key::ArrowLeft), Do::Seek(-5.0)),
            (plain(Key::ArrowRight), Do::Seek(5.0)),
        ],
    },
    Shortcut {
        group: Group::Playback,
        keys: &[&["Shift", "←"], &["Shift", "→"]],
        what: "Previous or next song",
        chords: &[
            (shift(Key::ArrowLeft), Do::Previous),
            (shift(Key::ArrowRight), Do::Next),
        ],
    },
    Shortcut {
        group: Group::Playback,
        keys: &[&["+"], &["-"]],
        what: "Volume up or down",
        chords: &[
            (either_shift(Key::Plus), Do::Volume(5.0)),
            (either_shift(Key::Equals), Do::Volume(5.0)),
            (plain(Key::Minus), Do::Volume(-5.0)),
        ],
    },
    Shortcut {
        group: Group::Playback,
        keys: &[&["M"]],
        what: "Mute or unmute",
        chords: &[(plain(Key::M), Do::Mute)],
    },
    Shortcut {
        group: Group::Playback,
        keys: &[&["S"]],
        what: "Shuffle on or off",
        chords: &[(plain(Key::S), Do::Shuffle)],
    },
    Shortcut {
        group: Group::Playback,
        keys: &[&["R"]],
        what: "Repeat: off, all or one",
        chords: &[(plain(Key::R), Do::Repeat)],
    },
    // Bound by the most-replayed seek bar (`Action::JumpToPeak`).
    Shortcut {
        group: Group::Playback,
        keys: &[&["P"]],
        what: "Jump to the most replayed part",
        chords: &[],
    },
    // Answered by Audition, on the song under the pointer.
    Shortcut {
        group: Group::Playback,
        keys: &[&["Hold Alt"]],
        what: "Audition the song under the pointer",
        chords: &[],
    },
    Shortcut {
        group: Group::Library,
        keys: &[&["L"]],
        what: "Like or unlike the playing song",
        chords: &[(plain(Key::L), Do::Like)],
    },
    Shortcut {
        group: Group::Library,
        keys: &[&["Ctrl", "K"]],
        what: "Play anything",
        chords: &[(ctrl(Key::K), Do::PlayAnything)],
    },
    Shortcut {
        group: Group::Library,
        keys: &[&["/"], &["Ctrl", "F"]],
        what: "Search",
        chords: &[(plain(Key::Slash), Do::Search), (ctrl(Key::F), Do::Search)],
    },
    Shortcut {
        group: Group::Navigation,
        keys: &[&["Alt", "←"], &["Alt", "→"]],
        what: "Back or forward",
        chords: &[
            (alt(Key::ArrowLeft), Do::Back),
            (alt(Key::ArrowRight), Do::Forward),
        ],
    },
    Shortcut {
        group: Group::Navigation,
        keys: &[&["Esc"]],
        what: "Close a menu, dialog or Now Playing",
        chords: &[(plain(Key::Escape), Do::Close)],
    },
    Shortcut {
        group: Group::Navigation,
        keys: &[&["Ctrl", "Q"]],
        what: "Quit",
        chords: &[(ctrl(Key::Q), Do::Quit)],
    },
    Shortcut {
        group: Group::Views,
        keys: &[&["N"]],
        what: "Open or close Now Playing",
        chords: &[(plain(Key::N), Do::NowPlaying)],
    },
    Shortcut {
        group: Group::Views,
        keys: &[&["Q"]],
        what: "Up next",
        chords: &[(plain(Key::Q), Do::UpNext)],
    },
    // Bound by Stage.
    Shortcut {
        group: Group::Views,
        keys: &[&["F"]],
        what: "Stage",
        chords: &[],
    },
    Shortcut {
        group: Group::Views,
        keys: &[&["E"]],
        what: "Equalizer",
        chords: &[(plain(Key::E), Do::Equalizer)],
    },
    Shortcut {
        group: Group::Views,
        keys: &[&["Ctrl", ","]],
        what: "Settings",
        chords: &[(ctrl(Key::Comma), Do::Settings)],
    },
    Shortcut {
        group: Group::Views,
        keys: &[&["Ctrl", "M"]],
        what: "Mini player",
        chords: &[(ctrl(Key::M), Do::Mini)],
    },
    Shortcut {
        group: Group::Views,
        keys: &[&["?"], &["Ctrl", "/"]],
        what: "Keyboard shortcuts",
        chords: &[
            (either_shift(Key::Questionmark), Do::Help),
            (shift(Key::Slash), Do::Help),
            (ctrl(Key::Slash), Do::Help),
        ],
    },
];

fn find(key: Key, modifiers: egui::Modifiers) -> Option<Do> {
    MAP.iter()
        .flat_map(|s| s.chords)
        .find(|(chord, _)| chord.matches(key, modifiers))
        .map(|(_, does)| *does)
}

/// The search field's id, so `/` and Ctrl+F can focus it.
pub(super) fn search_field() -> Id {
    Id::new("ytfast-search-field")
}

fn settings_flag() -> Id {
    Id::new("ytfast-open-settings")
}

/// Ctrl+, asked for Settings since the last call.
pub(super) fn settings_asked(ctx: &Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(settings_flag()))
        .unwrap_or(false)
}

fn typing_id() -> Id {
    Id::new("ytfast-typing")
}

/// Call once the frame is drawn: whether a text field has the keyboard.
/// egui lets go of a field on Esc before [`handle`] sees the key, so the
/// shortcuts go by how the last frame ended.
pub(super) fn end_frame(ctx: &Context) {
    let typing = ctx.text_edit_focused();
    ctx.data_mut(|d| d.insert_temp(typing_id(), typing));
}

/// What Esc closes now, if anything: the topmost of Play anything, the
/// shortcuts and Now Playing. Dialogs, the equalizer and popups close
/// themselves, and a text field just lets go.
fn close(app: &App, ctx: &Context, typing: bool) -> Option<Action> {
    if app.control.play_anything.open {
        Some(Action::Control(ControlAction::PlayAnything(false)))
    } else if app.control.help {
        Some(Action::Control(ControlAction::Help(false)))
    } else if typing
        || app.account_state.dialog.is_some()
        || app.equalizer_open
        || egui::Popup::is_any_open(ctx)
    {
        None
    } else if app.now_playing {
        Some(Action::NowPlaying(false))
    } else {
        None
    }
}

/// Answers this frame's shortcuts (call before anything else is drawn).
pub(super) fn handle(app: &App, ctx: &Context, actions: &mut Vec<Action>) {
    if super::menu::is_open(ctx) {
        // An open menu takes the arrows, Enter and Esc.
        super::menu::keys(ctx);
        return;
    }
    let typing = ctx.text_edit_focused() || ctx.data(|d| d.get_temp(typing_id()).unwrap_or(false));
    let mini = app.window == WindowKind::Mini;
    // Worked out before the input lock: it asks the context too.
    let mut closing = close(app, ctx, typing);
    let mut fired = Vec::new();
    ctx.input_mut(|i| {
        i.events.retain(|event| {
            let &Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } = event
            else {
                return true;
            };
            let Some(does) = find(key, modifiers) else {
                return true;
            };
            if (typing && !matches!(does, Do::Close | Do::PlayAnything))
                || (repeat && !does.repeats())
                || (mini && !matters_in_mini(does))
            {
                return true;
            }
            let action = if does == Do::Close {
                match closing.take() {
                    Some(action) => Some(action),
                    None => return true,
                }
            } else {
                None
            };
            fired.push((does, action));
            false
        });
    });
    let mut focusing = false;
    for (does, closing) in fired {
        let command = |c| Action::Command(c);
        let control = |c| Action::Control(c);
        let action = match does {
            Do::Close => closing,
            Do::TogglePause => (!app.queue.is_empty()).then(|| command(Command::TogglePause)),
            Do::Seek(by) => Some(control(ControlAction::SeekBy(by))),
            Do::Previous => Some(command(Command::Previous)),
            Do::Next => Some(command(Command::Next)),
            Do::Volume(by) => Some(control(ControlAction::VolumeBy(by))),
            Do::Mute => Some(control(ControlAction::Mute)),
            Do::Shuffle => Some(command(Command::ToggleShuffle)),
            Do::Repeat => Some(command(Command::CycleRepeat)),
            Do::Like => Some(Action::ToggleLikeCurrent),
            Do::Search => {
                focusing = true;
                ctx.memory_mut(|m| m.request_focus(search_field()));
                None
            }
            Do::PlayAnything => {
                focusing = true;
                let open = !app.control.play_anything.open;
                Some(control(ControlAction::PlayAnything(open)))
            }
            Do::Back if app.now_playing => Some(Action::NowPlaying(false)),
            Do::Back => Some(Action::Back),
            Do::Forward => Some(control(ControlAction::Forward)),
            Do::NowPlaying => Some(Action::NowPlaying(!app.now_playing)),
            Do::UpNext => {
                actions.push(Action::NowPlayingTab(NowPlayingTab::UpNext));
                Some(Action::NowPlaying(true))
            }
            Do::Equalizer => Some(Action::ShowEqualizer(!app.equalizer_open)),
            Do::Settings => {
                ctx.data_mut(|d| d.insert_temp(settings_flag(), true));
                None
            }
            Do::Mini => Some(Action::MiniPlayer(!mini)),
            Do::Help => Some(control(ControlAction::Help(!app.control.help))),
            Do::Quit => Some(Action::Quit),
        };
        actions.extend(action);
    }
    if focusing {
        // `/` also arrives as text: it must not land in the field it focuses.
        ctx.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Text(_))));
    }
}

/// The mini player answers playback keys, like, and its own switch and quit.
fn matters_in_mini(does: Do) -> bool {
    matches!(
        does,
        Do::TogglePause
            | Do::Seek(_)
            | Do::Previous
            | Do::Next
            | Do::Volume(_)
            | Do::Mute
            | Do::Shuffle
            | Do::Repeat
            | Do::Like
            | Do::Mini
            | Do::Quit
    )
}

/// The keyboard shortcuts over a dimmed window, grouped; Esc, `?` or a
/// click outside closes them.
pub(super) fn overlay(app: &App, ctx: &Context, p: &Palette, actions: &mut Vec<Action>) {
    let id = Id::new("ytfast-shortcuts");
    let shown = motion::spring(
        ctx,
        id.with("open"),
        if app.control.help { 1.0 } else { 0.0 },
        700.0,
    );
    if !app.control.help {
        return;
    }
    let screen = ctx.content_rect();
    let dim = egui::Area::new(id.with("dim"))
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .fade_in(false)
        .show(ctx, |ui| {
            ui.painter().rect_filled(
                screen,
                0.0,
                p.overlay.gamma_multiply(0.6 * shown.clamp(0.0, 1.0)),
            );
            ui.allocate_rect(screen, Sense::click())
        });
    if dim.inner.clicked() {
        actions.push(Action::Control(ControlAction::Help(false)));
    }
    let size = vec2(760.0_f32.min(screen.width() - 48.0), 560.0);
    let rect = Rect::from_center_size(screen.center(), size);
    let layer = LayerId::new(Order::Foreground, id);
    ctx.move_to_top(layer);
    // Grows into place from a little smaller, with weight.
    let scale = 0.94 + 0.06 * shown;
    let c = rect.center().to_vec2();
    ctx.set_transform_layer(
        layer,
        TSTransform::from_translation(c)
            * TSTransform::from_scaling(scale)
            * TSTransform::from_translation(-c),
    );
    egui::Area::new(id)
        .order(Order::Foreground)
        .fixed_pos(rect.min)
        .fade_in(false)
        .show(ctx, |ui| {
            ui.multiply_opacity(shown.clamp(0.0, 1.0));
            let response = ui.allocate_rect(rect, Sense::click());
            named(response, "Keyboard shortcuts");
            ui.painter().add(
                egui::epaint::Shadow {
                    offset: [0, 12],
                    blur: 40,
                    spread: 0,
                    color: p.shadow,
                }
                .as_shape(rect, CornerRadius::same(16)),
            );
            ui.painter().rect(
                rect,
                CornerRadius::same(16),
                p.panel,
                Stroke::new(1.0, p.outline),
                egui::StrokeKind::Inside,
            );
            let inner = rect.shrink2(vec2(32.0, 28.0));
            ui.painter().text(
                inner.left_top(),
                Align2::LEFT_TOP,
                "Keyboard shortcuts",
                font(Weight::Bold, 22.0),
                p.text,
            );
            ui.painter().text(
                inner.right_top() + vec2(0.0, 6.0),
                Align2::RIGHT_TOP,
                "Esc to close",
                font(Weight::Regular, 13.0),
                p.dim,
            );
            let top = inner.top() + 52.0;
            let half = (inner.width() - 40.0) / 2.0;
            let columns = [
                (inner.left(), &[Group::Playback][..]),
                (
                    inner.left() + half + 40.0,
                    &[Group::Library, Group::Navigation, Group::Views][..],
                ),
            ];
            for (x, groups) in columns {
                let mut y = top;
                for group in groups {
                    ui.painter().text(
                        pos2(x, y),
                        Align2::LEFT_TOP,
                        group.title(),
                        font(Weight::SemiBold, 13.0),
                        p.secondary,
                    );
                    y += 26.0;
                    for s in MAP.iter().filter(|s| s.group == *group) {
                        row(ui, s, pos2(x, y), half, p);
                        y += 34.0;
                    }
                    y += 14.0;
                }
            }
        });
}

/// One shortcut: its caps on the left, what it does beside them.
fn row(ui: &mut Ui, s: &Shortcut, at: egui::Pos2, width: f32, p: &Palette) {
    const CAPS: f32 = 132.0;
    let mut x = at.x;
    let mid = at.y + 13.0;
    for (n, combo) in s.keys.iter().enumerate() {
        if n > 0 {
            let slash = ui.painter().text(
                pos2(x + 4.0, mid),
                Align2::LEFT_CENTER,
                "/",
                font(Weight::Regular, 13.0),
                p.dim,
            );
            x = slash.right() + 4.0;
        }
        for cap in combo.iter() {
            let galley =
                ui.painter()
                    .layout_no_wrap((*cap).to_owned(), font(Weight::Medium, 12.5), p.text);
            let w = (galley.size().x + 14.0).max(26.0);
            let key = Rect::from_min_size(pos2(x, mid - 12.0), vec2(w, 24.0));
            ui.painter().rect(
                key,
                CornerRadius::same(6),
                p.surface,
                Stroke::new(1.0, p.outline),
                egui::StrokeKind::Inside,
            );
            ui.painter()
                .galley(key.center() - galley.size() / 2.0, galley, p.text);
            x = key.right() + 4.0;
        }
    }
    let text_x = (at.x + CAPS).max(x + 8.0);
    let mut text = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
        pos2(text_x, mid - 10.0),
        Vec2::new((at.x + width - text_x).max(40.0), 20.0),
    )));
    label(&mut text, s.what, 14.0, Weight::Regular, p.text);
}
