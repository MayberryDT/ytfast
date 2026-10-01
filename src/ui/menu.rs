//! Context menus (docs/SPEC.md § Control): right-clicking a song, album,
//! playlist or artist (or its ⋮ button) opens its actions. A menu grows out
//! of where it was opened on a spring, and the keyboard drives it: arrows
//! move, Enter chooses, Esc closes.
//!
//! Album, playlist and artist pages are fetched as their menu opens, so
//! Play next and Add to queue answer at once and the items that depend on
//! the page (Save to library, Subscribe) show as soon as it is known.

use super::motion;
use super::widgets::{font, named};
use crate::account::{AccountAction, Dialog};
use crate::app::{Action, App};
use crate::backend::Command;
use crate::control::{ControlAction, FromPage};
use crate::icons::Icon;
use crate::model::{Account, Header, Item, ItemKind, LikeStatus, Run, Target, Track};
use crate::theme::Palette;
use egui::{
    Align2, Context, CornerRadius, Event, Id, Key, LayerId, Order, Pos2, Rect, Sense, Stroke, Ui,
    Vec2, emath::TSTransform, pos2, vec2,
};
use fastframe_fonts::Weight;

/// Where a song's menu was opened: what else it can offer.
#[derive(Clone, Debug)]
pub(super) enum Place {
    List,
    /// A row of the account's own playlist: Remove from playlist.
    Own {
        playlist_id: String,
        set_video_id: String,
    },
    /// Up next, at this queue position: Remove from queue.
    UpNext(usize),
    Player,
}

/// What a menu is about.
#[derive(Clone, Debug)]
pub(super) enum Subject {
    Song {
        track: Track,
        place: Place,
    },
    Collection {
        page: Option<Target>,
        /// A card's play target (an album's audio playlist, a mix).
        play: Option<Target>,
        /// Linked artists in its subtitle.
        artists: Vec<Run>,
    },
    Artist {
        page: Target,
    },
}

#[derive(Clone)]
struct Menu {
    subject: Subject,
    at: Pos2,
    /// The item the keyboard is on.
    selected: Option<usize>,
    /// Entries last frame, for the arrows to wrap.
    count: usize,
    /// Enter chose the selected entry.
    chosen: bool,
    close: bool,
    opened: u64,
    /// Where the menu was drawn last frame, for clicks outside it.
    rect: Option<Rect>,
}

fn state_id() -> Id {
    Id::new("ytfast-context-menu")
}

fn spring_id() -> Id {
    state_id().with("open")
}

fn get(ctx: &Context) -> Option<Menu> {
    ctx.data(|d| d.get_temp::<Menu>(state_id()))
}

fn put(ctx: &Context, menu: Option<Menu>) {
    ctx.data_mut(|d| match menu {
        Some(menu) => {
            d.insert_temp(state_id(), menu);
        }
        None => d.remove::<Menu>(state_id()),
    });
}

pub(super) fn is_open(ctx: &Context) -> bool {
    get(ctx).is_some()
}

/// Opens `subject`'s menu at `at`. A page it depends on starts loading.
pub(super) fn open(ctx: &Context, subject: Subject, at: Pos2, actions: &mut Vec<Action>) {
    let page = match &subject {
        Subject::Collection { page, .. } => page.clone(),
        Subject::Artist { page } => Some(page.clone()),
        Subject::Song { .. } => None,
    };
    if let Some(page) = page {
        actions.push(Action::Load(page));
    }
    put(
        ctx,
        Some(Menu {
            subject,
            at,
            selected: None,
            count: 0,
            chosen: false,
            close: false,
            opened: ctx.cumulative_pass_nr(),
            rect: None,
        }),
    );
}

/// The menu for a card or row, if it is music.
pub(super) fn subject_of(item: &Item, place: Place) -> Option<Subject> {
    if let Some(track) = &item.track {
        let mut track = track.clone();
        if track.thumbnail.is_none() {
            track.thumbnail.clone_from(&item.thumbnail);
        }
        return Some(Subject::Song { track, place });
    }
    let browse = item
        .target
        .clone()
        .filter(|t| matches!(t, Target::Browse { .. }));
    match item.kind {
        ItemKind::Album | ItemKind::Playlist => Some(Subject::Collection {
            page: browse,
            play: item.play.clone().or_else(|| {
                item.target
                    .clone()
                    .filter(|t| matches!(t, Target::Watch { .. }))
            }),
            artists: artist_runs(&item.subtitle),
        }),
        ItemKind::Artist => Some(Subject::Artist { page: browse? }),
        _ => None,
    }
}

/// The menu for a page's header: the album, playlist or artist itself.
pub(super) fn subject_of_header(header: &Header, page: &Target) -> Subject {
    let artist = header.subscription.is_some()
        || matches!(page, Target::Browse { id, .. } if id.starts_with("UC"));
    if artist {
        Subject::Artist { page: page.clone() }
    } else {
        Subject::Collection {
            page: Some(page.clone()),
            play: header.play.clone(),
            artists: artist_runs(&header.subtitle),
        }
    }
}

/// Runs that link to an artist's page.
fn artist_runs(runs: &[Run]) -> Vec<Run> {
    runs.iter()
        .filter(|r| matches!(&r.target, Some(Target::Browse { id, .. }) if id.starts_with("UC")))
        .cloned()
        .collect()
}

/// A right-click on `response` opens its menu at the pointer.
pub(super) fn on_secondary(
    ui: &Ui,
    response: &egui::Response,
    subject: impl FnOnce() -> Option<Subject>,
    actions: &mut Vec<Action>,
) {
    if response.secondary_clicked()
        && let Some(subject) = subject()
    {
        let at = response
            .interact_pointer_pos()
            .or_else(|| ui.ctx().pointer_latest_pos())
            .unwrap_or(response.rect.center());
        open(ui.ctx(), subject, at, actions);
    }
}

/// The ⋮ button over `rect`; named "More actions for …" for screen
/// readers and the E2E driver.
pub(super) fn dots(ui: &mut Ui, rect: Rect, id: Id, title: &str, p: &Palette) -> egui::Response {
    let response = ui.interact(rect, id, Sense::click());
    if response.hovered() {
        ui.painter().circle_filled(
            rect.center(),
            rect.width().min(rect.height()) / 2.0,
            p.surface_active,
        );
    }
    Icon::More
        .image(p.text, 18.0)
        .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(18.0)));
    named(response, &format!("More actions for {title}"))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("More actions")
}

/// A card's ⋮, drawn in its cover's corner while the card is lifted
/// (`shown` 0 → 1); returns whether the pointer is on it.
pub(super) fn card_dots(
    ui: &Ui,
    center: Pos2,
    shown: f32,
    hovered: bool,
    item: &Item,
    p: &Palette,
) -> bool {
    if shown <= 0.01 || subject_of(item, Place::List).is_none() {
        return false;
    }
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let hit = hovered && pointer.is_some_and(|pos| pos.distance(center) < 16.0);
    let fill = if hit { p.surface_active } else { p.overlay };
    ui.painter().circle_filled(
        center,
        16.0 * shown.min(1.0),
        fill.gamma_multiply(shown.min(1.0)),
    );
    Icon::More
        .image(p.text.gamma_multiply(shown.min(1.0)), 18.0)
        .paint_at(ui, Rect::from_center_size(center, Vec2::splat(18.0)));
    #[cfg(feature = "e2e")]
    crate::e2e::register(
        ui.ctx(),
        &format!("More actions for {}", item.title),
        Rect::from_center_size(center, Vec2::splat(28.0)),
    );
    hit
}

/// Room a row without a length (an album, playlist or artist) keeps on its
/// right for its ⋮.
pub(super) fn row_reserve(item: &Item) -> f32 {
    let menu = matches!(
        item.kind,
        ItemKind::Album | ItemKind::Playlist | ItemKind::Artist
    );
    if item.track.is_none() && menu {
        40.0
    } else {
        0.0
    }
}

/// A song row's ⋮, in place of its length while the row is under the
/// pointer, and its right-click; returns whether the ⋮ shows.
pub(super) fn row(
    ui: &mut Ui,
    rect: Rect,
    response: &egui::Response,
    item: &Item,
    place: impl Fn() -> Place,
    actions: &mut Vec<Action>,
    p: &Palette,
) -> bool {
    on_secondary(ui, response, || subject_of(item, place()), actions);
    // Song rows keep their duration's place for it; rows without a length
    // have their own controls there.
    let room = item.track.as_ref().is_none_or(|t| t.duration.is_some());
    if !room || !response.contains_pointer() || subject_of(item, Place::List).is_none() {
        return false;
    }
    let button = Rect::from_center_size(
        pos2(rect.right() - 36.0, rect.center().y),
        Vec2::splat(32.0),
    );
    if dots(ui, button, response.id.with("menu"), &item.title, p).clicked()
        && let Some(subject) = subject_of(item, place())
    {
        open(ui.ctx(), subject, button.left_bottom(), actions);
    }
    true
}

/// An open menu's keys (taken before anything else sees them).
pub(super) fn keys(ctx: &Context) {
    let Some(mut menu) = get(ctx) else { return };
    ctx.input_mut(|i| {
        i.events.retain(|event| {
            let &Event::Key {
                key, pressed: true, ..
            } = event
            else {
                return !matches!(
                    event,
                    Event::Key {
                        key: Key::ArrowDown | Key::ArrowUp | Key::Enter | Key::Escape | Key::Tab,
                        ..
                    }
                );
            };
            let count = menu.count.max(1);
            match key {
                Key::ArrowDown | Key::Tab => {
                    menu.selected = Some(menu.selected.map_or(0, |s| (s + 1) % count));
                }
                Key::ArrowUp => {
                    menu.selected =
                        Some(menu.selected.map_or(count - 1, |s| (s + count - 1) % count));
                }
                Key::Enter | Key::Space => menu.chosen = menu.selected.is_some(),
                Key::Escape => menu.close = true,
                _ => return true,
            }
            false
        });
    });
    put(ctx, Some(menu));
}

/// What an entry does.
#[derive(Clone)]
enum Does {
    PlayNext,
    AddToQueue,
    SongRadio,
    Rate(LikeStatus),
    AddToPlaylist,
    Open(Target),
    Copy(String),
    RemoveOwn {
        playlist_id: String,
        set_video_id: String,
    },
    RemoveQueued(usize),
    Page(FromPage),
    Play(Target),
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
}

struct Entry {
    icon: Icon,
    label: &'static str,
    does: Does,
}

fn entry(icon: Icon, label: &'static str, does: Does) -> Entry {
    Entry { icon, label, does }
}

const MUSIC: &str = "https://music.youtube.com";

fn page_link(page: Option<&Target>, play: Option<&Target>) -> Option<String> {
    match (page, play) {
        (Some(Target::Browse { id, .. }), _) => Some(match id.strip_prefix("VL") {
            Some(list) => format!("{MUSIC}/playlist?list={list}"),
            None if id.starts_with("UC") => format!("{MUSIC}/channel/{id}"),
            None => format!("{MUSIC}/browse/{id}"),
        }),
        (
            _,
            Some(Target::Watch {
                playlist_id: Some(list),
                ..
            }),
        ) => Some(format!("{MUSIC}/playlist?list={list}")),
        _ => None,
    }
}

/// The menu's entries now: those that depend on the account or on a page
/// still loading appear once they are known.
fn entries(app: &App, subject: &Subject) -> Vec<Entry> {
    let signed_in = matches!(app.account, Account::SignedIn { .. });
    let marks = &app.account_state.marks;
    let mut out = Vec::new();
    match subject {
        Subject::Song { track, place } => {
            out.push(entry(Icon::PlayNext, "Play next", Does::PlayNext));
            out.push(entry(Icon::AddToQueue, "Add to queue", Does::AddToQueue));
            out.push(entry(Icon::Radio, "Start radio", Does::SongRadio));
            if signed_in {
                let liked = marks.like(track) == LikeStatus::Like;
                out.push(if liked {
                    entry(
                        Icon::ThumbsUpFilled,
                        "Unlike",
                        Does::Rate(LikeStatus::Indifferent),
                    )
                } else {
                    entry(Icon::ThumbsUp, "Like", Does::Rate(LikeStatus::Like))
                });
                out.push(entry(
                    Icon::AddToPlaylist,
                    "Add to playlist",
                    Does::AddToPlaylist,
                ));
            }
            if let Some(album) = track.album.as_ref().and_then(|a| a.target.clone()) {
                out.push(entry(Icon::Album, "Go to album", Does::Open(album)));
            }
            if let Some(artist) = track.artists.iter().find_map(|a| a.target.clone()) {
                out.push(entry(Icon::User, "Go to artist", Does::Open(artist)));
            }
            out.push(entry(
                Icon::Link,
                "Copy link",
                Does::Copy(format!("{MUSIC}/watch?v={}", track.video_id)),
            ));
            match place {
                Place::Own {
                    playlist_id,
                    set_video_id,
                } if signed_in => out.push(entry(
                    Icon::Trash,
                    "Remove from playlist",
                    Does::RemoveOwn {
                        playlist_id: playlist_id.clone(),
                        set_video_id: set_video_id.clone(),
                    },
                )),
                Place::UpNext(i) if app.playback.index != Some(*i) => out.push(entry(
                    Icon::RemoveFromQueue,
                    "Remove from queue",
                    Does::RemoveQueued(*i),
                )),
                _ => {}
            }
        }
        Subject::Collection {
            page,
            play,
            artists,
        } => {
            let header = page
                .as_ref()
                .and_then(|t| app.page_state(t)?.page.as_ref()?.header.as_ref());
            match page {
                Some(_) => {
                    out.push(entry(
                        Icon::PlayNext,
                        "Play next",
                        Does::Page(FromPage::Next),
                    ));
                    out.push(entry(
                        Icon::AddToQueue,
                        "Add to queue",
                        Does::Page(FromPage::Queue),
                    ));
                    out.push(entry(
                        Icon::Shuffle,
                        "Shuffle play",
                        Does::Page(FromPage::Shuffle),
                    ));
                    out.push(entry(
                        Icon::Radio,
                        "Start radio",
                        Does::Page(FromPage::Radio),
                    ));
                }
                // A mix: it plays, nothing more.
                None => {
                    if let Some(play) = play {
                        out.push(entry(Icon::Play, "Play", Does::Play(play.clone())));
                    }
                }
            }
            if let (true, Some(h)) = (signed_in, header)
                && let Some(library) = &h.library
            {
                let saved = marks.saved(library);
                out.push(entry(
                    if saved { Icon::Check } else { Icon::Library },
                    if saved {
                        "Remove from library"
                    } else {
                        "Save to library"
                    },
                    Does::Save {
                        playlist_id: library.playlist_id.clone(),
                        title: h.title.clone(),
                        save: !saved,
                    },
                ));
            }
            if let Some(artist) = artists.iter().find_map(|r| r.target.clone()) {
                out.push(entry(Icon::User, "Go to artist", Does::Open(artist)));
            }
            if let Some(link) = page_link(page.as_ref(), play.as_ref()) {
                out.push(entry(Icon::Link, "Copy link", Does::Copy(link)));
            }
        }
        Subject::Artist { page } => {
            out.push(entry(
                Icon::Radio,
                "Start radio",
                Does::Page(FromPage::Radio),
            ));
            let header = app
                .page_state(page)
                .and_then(|s| s.page.as_ref()?.header.as_ref());
            if signed_in
                && let Some((sub, name)) =
                    header.and_then(|h| Some((h.subscription.as_ref()?, h.title.clone())))
            {
                let subscribed = marks.subscribed(sub);
                out.push(entry(
                    if subscribed {
                        Icon::BellOff
                    } else {
                        Icon::Bell
                    },
                    if subscribed {
                        "Unsubscribe"
                    } else {
                        "Subscribe"
                    },
                    Does::Subscribe {
                        channel_id: sub.channel_id.clone(),
                        name,
                        subscribe: !subscribed,
                    },
                ));
            }
            if let Some(link) = page_link(Some(page), None) {
                out.push(entry(Icon::Link, "Copy link", Does::Copy(link)));
            }
        }
    }
    out
}

fn perform(subject: &Subject, does: Does, actions: &mut Vec<Action>) {
    let account = |a| Action::Account(a);
    match (does, subject) {
        (Does::PlayNext, Subject::Song { track, .. }) => {
            actions.push(Action::Command(Command::PlayNext(vec![track.clone()])));
        }
        (Does::AddToQueue, Subject::Song { track, .. }) => {
            actions.push(Action::Command(Command::AddToQueue(vec![track.clone()])));
        }
        (Does::SongRadio, Subject::Song { track, .. }) => {
            // YouTube Music's radio for a song, as autoplay continues with.
            actions.push(Action::Command(Command::PlayTarget(Target::Watch {
                video_id: Some(track.video_id.clone()),
                playlist_id: Some(format!("RDAMVM{}", track.video_id)),
                params: Some("wAEB".into()),
            })));
        }
        (Does::Rate(status), Subject::Song { track, .. }) => {
            actions.push(account(AccountAction::Rate {
                track: track.clone(),
                status,
            }));
        }
        (Does::AddToPlaylist, Subject::Song { track, .. }) => {
            actions.push(account(AccountAction::Dialog(Some(
                Dialog::AddToPlaylist {
                    tracks: vec![track.clone()],
                    filter: String::new(),
                    selected: 0,
                },
            ))));
        }
        (
            Does::Page(how),
            Subject::Collection {
                page: Some(page), ..
            }
            | Subject::Artist { page },
        ) => actions.push(Action::Control(ControlAction::FromPage {
            page: page.clone(),
            how,
        })),
        (Does::Open(target) | Does::Play(target), _) => actions.push(Action::Activate(target)),
        (Does::Copy(link), _) => actions.push(Action::Control(ControlAction::CopyLink(link))),
        (
            Does::RemoveOwn {
                playlist_id,
                set_video_id,
            },
            _,
        ) => actions.push(account(AccountAction::Remove {
            playlist_id,
            set_video_id,
        })),
        (Does::RemoveQueued(i), _) => {
            actions.push(Action::Command(Command::RemoveFromQueue(i)));
        }
        (
            Does::Save {
                playlist_id,
                title,
                save,
            },
            _,
        ) => actions.push(account(AccountAction::Save {
            playlist_id,
            title,
            save,
        })),
        (
            Does::Subscribe {
                channel_id,
                name,
                subscribe,
            },
            _,
        ) => actions.push(account(AccountAction::Subscribe {
            channel_id,
            name,
            subscribe,
        })),
        _ => {}
    }
}

const ROW: f32 = 38.0;
const WIDTH: f32 = 248.0;

/// The open menu, above everything (call after the rest is drawn).
pub(super) fn show(app: &App, ctx: &Context, p: &Palette, actions: &mut Vec<Action>) {
    let Some(mut menu) = get(ctx) else {
        // Rests at closed, so the next menu grows from nothing.
        motion::spring(ctx, spring_id(), 0.0, 900.0);
        return;
    };
    let entries = entries(app, &menu.subject);
    menu.count = entries.len();
    if let Some(s) = menu.selected {
        menu.selected = Some(s.min(entries.len().saturating_sub(1)));
    }
    if menu.close {
        put(ctx, None);
        return;
    }
    if menu.chosen
        && let Some(e) = menu.selected.and_then(|s| entries.get(s))
    {
        perform(&menu.subject, e.does.clone(), actions);
        put(ctx, None);
        return;
    }
    // A press elsewhere, or scrolling, closes it.
    let (pressed, scrolled) = ctx.input(|i| {
        (
            i.pointer
                .any_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten(),
            i.raw
                .events
                .iter()
                .any(|e| matches!(e, Event::MouseWheel { .. })),
        )
    });
    let outside = pressed.is_some_and(|pos| menu.rect.is_none_or(|r| !r.contains(pos)));
    if ctx.cumulative_pass_nr() > menu.opened && (outside || scrolled) {
        put(ctx, None);
        return;
    }

    let t = motion::spring(ctx, spring_id(), 1.0, 900.0).clamp(0.0, 1.1);
    let id = state_id().with("area");
    let layer = LayerId::new(Order::Foreground, id);
    let screen = ctx.content_rect();
    let height = entries.len() as f32 * ROW + 12.0;
    // Opens below and right of the pointer, or wherever it fits.
    let x = if menu.at.x + WIDTH > screen.right() - 8.0 {
        menu.at.x - WIDTH
    } else {
        menu.at.x
    };
    let y = if menu.at.y + height > screen.bottom() - 8.0 {
        (menu.at.y - height).max(screen.top() + 8.0)
    } else {
        menu.at.y
    };
    let rect = Rect::from_min_size(pos2(x, y), vec2(WIDTH, height));
    // It grows out of the point it was opened from.
    let pivot = menu.at.to_vec2();
    let scale = 0.88 + 0.12 * t;
    ctx.set_transform_layer(
        layer,
        TSTransform::from_translation(pivot)
            * TSTransform::from_scaling(scale)
            * TSTransform::from_translation(-pivot),
    );
    ctx.move_to_top(layer);
    let mut chosen = None;
    egui::Area::new(id)
        .order(Order::Foreground)
        .fixed_pos(rect.min)
        .fade_in(false)
        .show(ctx, |ui| {
            ui.multiply_opacity(t.min(1.0));
            ui.painter().add(
                egui::epaint::Shadow {
                    offset: [0, 8],
                    blur: 24,
                    spread: 0,
                    color: p.shadow,
                }
                .as_shape(rect, CornerRadius::same(10)),
            );
            ui.painter().rect(
                rect,
                CornerRadius::same(10),
                p.panel,
                Stroke::new(1.0, p.outline),
                egui::StrokeKind::Inside,
            );
            for (i, e) in entries.iter().enumerate() {
                let row = Rect::from_min_size(
                    pos2(rect.left() + 6.0, rect.top() + 6.0 + i as f32 * ROW),
                    vec2(WIDTH - 12.0, ROW),
                );
                let response = ui.interact(row, id.with(i), Sense::click());
                if response.hovered() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
                    menu.selected = Some(i);
                }
                if menu.selected == Some(i) || response.hovered() {
                    ui.painter()
                        .rect_filled(row, CornerRadius::same(6), p.surface_hover);
                }
                e.icon.image(p.secondary, 18.0).paint_at(
                    ui,
                    Rect::from_center_size(
                        pos2(row.left() + 20.0, row.center().y),
                        Vec2::splat(18.0),
                    ),
                );
                ui.painter().text(
                    pos2(row.left() + 42.0, row.center().y),
                    Align2::LEFT_CENTER,
                    e.label,
                    font(Weight::Medium, 14.0),
                    p.text,
                );
                if named(response, e.label)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    chosen = Some(i);
                }
            }
        });
    if let Some(e) = chosen.and_then(|i| entries.get(i)) {
        perform(&menu.subject, e.does.clone(), actions);
        put(ctx, None);
        return;
    }
    menu.rect = Some(rect);
    put(ctx, Some(menu));
}
