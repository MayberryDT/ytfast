//! The account's controls: like and dislike, Save to library, Subscribe,
//! the account's own playlists (edit, delete, remove and reorder songs), the
//! Add to playlist picker, and dragging songs onto playlists. Changes go out
//! as [`AccountAction`]s; `crate::account` shows them at once and rolls them
//! back if YouTube Music refuses.

use std::sync::Arc;

use super::widgets::{font, label, pill};
use crate::account::{AccountAction, Dialog, Marks};
use crate::app::{Action, App, LibraryTab, View};
use crate::icons::Icon;
use crate::model::{Account, Header, Item, LikeStatus, Track};
use crate::theme::Palette;
use egui::{
    Align, Align2, Color32, CornerRadius, Id, Key, Layout, Rect, RichText, Sense, Stroke, Ui, Vec2,
    pos2, vec2,
};
use fastframe_fonts::Weight;

/// Width of one row control.
const SLOT: f32 = 36.0;

/// What the views need to know about the account this frame.
struct Shared {
    marks: Arc<Marks>,
    signed_in: bool,
    /// The account's own playlist on screen, whose rows can be removed and moved.
    editable: Option<String>,
}

/// A song being dragged: onto a sidebar playlist (add) or within its own
/// playlist (move).
struct SongDrag {
    track: Track,
    /// The own playlist it is dragged within, if any.
    playlist: Option<String>,
}

fn shared_id() -> Id {
    Id::new("ytfast-account-shared")
}

/// Hands this frame's account state to the views (call before drawing).
pub(super) fn publish(app: &App, ui: &Ui) {
    let editable = match &app.view {
        View::Page(target) if !app.now_playing => app
            .page_state(target)
            .and_then(|s| s.page.as_ref())
            .and_then(|p| p.header.as_ref())
            .and_then(|h| h.editable.clone()),
        _ => None,
    };
    let shared = Arc::new(Shared {
        marks: app.account_state.marks.clone(),
        signed_in: matches!(app.account, Account::SignedIn { .. }),
        editable,
    });
    ui.ctx().data_mut(|d| d.insert_temp(shared_id(), shared));
}

fn shared(ui: &Ui) -> Option<Arc<Shared>> {
    ui.ctx()
        .data(|d| d.get_temp::<Arc<Shared>>(shared_id()))
        .filter(|s| s.signed_in)
}

/// Names a control for screen readers and the E2E driver; toggles carry
/// their pressed state.
fn describe(response: &egui::Response, label: &str, selected: Option<bool>) {
    let text = label.to_owned();
    match selected {
        Some(on) => response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, &text)
        }),
        None => response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &text)),
    }
    #[cfg(feature = "e2e")]
    crate::e2e::register(&response.ctx, label, response.interact_rect);
}

/// An icon button over `rect`, with a hover disc.
#[allow(clippy::too_many_arguments)]
fn icon_at(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    icon: Icon,
    size: f32,
    color: Color32,
    hover: Color32,
    label: &str,
    selected: Option<bool>,
) -> egui::Response {
    let response = ui.interact(rect, id, Sense::click());
    if response.hovered() {
        ui.painter()
            .circle_filled(rect.center(), rect.width().min(rect.height()) / 2.0, hover);
    }
    icon.image(color, size)
        .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(size)));
    describe(&response, label, selected);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(label)
}

fn like_icons(status: LikeStatus) -> (Icon, Icon) {
    match status {
        LikeStatus::Like => (Icon::ThumbsUpFilled, Icon::ThumbsDown),
        LikeStatus::Dislike => (Icon::ThumbsUp, Icon::ThumbsDownFilled),
        LikeStatus::Indifferent => (Icon::ThumbsUp, Icon::ThumbsDown),
    }
}

/// Dislike, Like and Add to playlist for the playing song, laid out left to
/// right in `ui`.
fn song_buttons(
    ui: &mut Ui,
    s: &Shared,
    track: &Track,
    size: f32,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    let status = s.marks.like(track);
    let (up, down) = like_icons(status);
    let side = size + 16.0;
    let disliked = status == LikeStatus::Dislike;
    let liked = status == LikeStatus::Like;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    let color = |on: bool| if on { p.text } else { p.secondary };
    if icon_at(
        ui,
        rect,
        Id::new(("dislike", ui.id())),
        down,
        size,
        color(disliked),
        p.surface_hover,
        "Dislike",
        Some(disliked),
    )
    .clicked()
    {
        actions.push(Action::Account(AccountAction::Rate {
            track: track.clone(),
            status: if disliked {
                LikeStatus::Indifferent
            } else {
                LikeStatus::Dislike
            },
        }));
    }
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    if icon_at(
        ui,
        rect,
        Id::new(("like", ui.id())),
        up,
        size,
        color(liked),
        p.surface_hover,
        "Like",
        Some(liked),
    )
    .clicked()
    {
        actions.push(Action::Account(AccountAction::Rate {
            track: track.clone(),
            status: if liked {
                LikeStatus::Indifferent
            } else {
                LikeStatus::Like
            },
        }));
    }
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    if icon_at(
        ui,
        rect,
        Id::new(("add", ui.id())),
        Icon::AddToPlaylist,
        size,
        p.secondary,
        p.surface_hover,
        "Add to playlist",
        None,
    )
    .clicked()
    {
        actions.push(open_picker(vec![track.clone()]));
    }
}

fn open_picker(tracks: Vec<Track>) -> Action {
    Action::Account(AccountAction::Dialog(Some(Dialog::AddToPlaylist {
        tracks,
        filter: String::new(),
        selected: 0,
    })))
}

/// The player bar's song controls, at the right end of `area`; returns the
/// width they take.
pub(super) fn player_controls(
    ui: &mut Ui,
    area: Rect,
    track: &Track,
    p: &Palette,
    actions: &mut Vec<Action>,
) -> f32 {
    let Some(s) = shared(ui) else { return 0.0 };
    let width = 3.0 * SLOT;
    let rect = Rect::from_min_max(
        pos2(area.right() - width - 16.0, area.top()),
        pos2(area.right() - 16.0, area.bottom()),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 0.0;
    song_buttons(&mut child, &s, track, 20.0, p, actions);
    width + 16.0
}

/// Now Playing's song controls, centred under the title.
pub(super) fn now_playing_controls(
    ui: &mut Ui,
    track: &Track,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    let Some(s) = shared(ui) else { return };
    let size = vec2(3.0 * (22.0 + 16.0), 40.0);
    let rect = Rect::from_center_size(
        pos2(ui.max_rect().center().x, ui.cursor().top() + size.y / 2.0),
        size,
    );
    ui.allocate_rect(rect, Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 0.0;
    song_buttons(&mut child, &s, track, 22.0, p, actions);
    ui.add_space(8.0);
}

/// How a song row senses input: songs can be dragged onto playlists.
pub(super) fn row_sense(ui: &Ui, item: &Item) -> Sense {
    if item.track.is_some() && shared(ui).is_some() {
        Sense::click_and_drag()
    } else {
        Sense::click()
    }
}

/// Room a song row keeps free on its right for its controls.
pub(super) fn row_reserve(ui: &Ui, item: &Item) -> f32 {
    let (Some(s), Some(track)) = (shared(ui), &item.track) else {
        return 0.0;
    };
    if s.editable.is_some() && track.set_video_id.is_some() {
        3.0 * SLOT
    } else {
        2.0 * SLOT
    }
}

/// The account's own playlist on screen and this row's entry in it, for a
/// menu's Remove from playlist: (playlist id, entry id).
pub(super) fn own_entry(ui: &Ui, item: &Item) -> Option<(String, String)> {
    let s = shared(ui)?;
    Some((
        s.editable.clone()?,
        item.track.as_ref()?.set_video_id.clone()?,
    ))
}

/// A song row's controls: Like (shown while hovered, or when liked), Add to
/// playlist, and on the account's own playlist Remove; dragging the row
/// onto a sidebar playlist adds it there, onto another row of its own
/// playlist moves it.
pub(super) fn row_controls(
    ui: &mut Ui,
    rect: Rect,
    response: &egui::Response,
    item: &Item,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    let (Some(s), Some(track)) = (shared(ui), &item.track) else {
        return;
    };
    let entry = track.set_video_id.as_deref();
    let own = s.editable.as_deref().filter(|_| entry.is_some());
    let dragging = egui::DragAndDrop::has_any_payload(ui.ctx());
    let hovered = response.contains_pointer() && !dragging;

    // Drag: carry the song; while held, a chip follows the pointer.
    if response.drag_started() {
        egui::DragAndDrop::set_payload(
            ui.ctx(),
            SongDrag {
                track: track.clone(),
                playlist: own.map(str::to_owned),
            },
        );
    }
    if response.dragged()
        && let Some(pos) = ui.ctx().pointer_latest_pos()
    {
        drag_chip(ui.ctx(), pos, &item.title, p);
    }
    // Drop within its own playlist: move.
    if let (Some(playlist), Some(entry)) = (own, entry) {
        let same = |d: &SongDrag| {
            d.playlist.as_deref() == Some(playlist)
                && d.track.set_video_id.as_deref() != Some(entry)
        };
        if response
            .dnd_hover_payload::<SongDrag>()
            .is_some_and(|d| same(&d))
        {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(6),
                Stroke::new(2.0, p.accent),
                egui::StrokeKind::Inside,
            );
        }
        if let Some(dropped) = response.dnd_release_payload::<SongDrag>()
            && same(&dropped)
            && let Some(moved) = dropped.track.set_video_id.clone()
        {
            actions.push(Action::Account(AccountAction::Move {
                playlist_id: playlist.to_owned(),
                set_video_id: moved,
                onto: entry.to_owned(),
            }));
        }
    }

    let status = s.marks.like(track);
    let liked = status == LikeStatus::Like;
    if !hovered && !liked {
        return;
    }
    let right = rect.right() - if track.duration.is_some() { 72.0 } else { 16.0 };
    let slot = |n: f32| {
        Rect::from_min_size(
            pos2(right - SLOT * (n + 1.0), rect.center().y - SLOT / 2.0),
            Vec2::splat(SLOT),
        )
    };
    let hover = p.surface_active;
    let title = &item.title;
    let (up, _) = like_icons(status);
    let like_label = format!("Like “{title}”");
    if icon_at(
        ui,
        slot(0.0),
        response.id.with("like"),
        up,
        18.0,
        if liked { p.text } else { p.secondary },
        hover,
        &like_label,
        Some(liked),
    )
    .clicked()
    {
        actions.push(Action::Account(AccountAction::Rate {
            track: track.clone(),
            status: if liked {
                LikeStatus::Indifferent
            } else {
                LikeStatus::Like
            },
        }));
    }
    if !hovered {
        return;
    }
    let add_label = format!("Add “{title}” to a playlist");
    if icon_at(
        ui,
        slot(1.0),
        response.id.with("add"),
        Icon::AddToPlaylist,
        18.0,
        p.secondary,
        hover,
        &add_label,
        None,
    )
    .clicked()
    {
        actions.push(open_picker(vec![track.clone()]));
    }
    if let (Some(playlist), Some(entry)) = (own, entry) {
        let remove_label = format!("Remove “{title}” from the playlist");
        if icon_at(
            ui,
            slot(2.0),
            response.id.with("remove"),
            Icon::Trash,
            18.0,
            p.secondary,
            hover,
            &remove_label,
            None,
        )
        .clicked()
        {
            actions.push(Action::Account(AccountAction::Remove {
                playlist_id: playlist.to_owned(),
                set_video_id: entry.to_owned(),
            }));
        }
    }
}

fn drag_chip(ctx: &egui::Context, pos: egui::Pos2, title: &str, p: &Palette) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        Id::new("song-drag"),
    ));
    let galley = painter.layout_no_wrap(title.to_owned(), font(Weight::Medium, 14.0), p.text);
    let rect = Rect::from_min_size(
        pos + vec2(14.0, 10.0),
        vec2(galley.size().x.min(320.0) + 36.0, 34.0),
    );
    painter.rect(
        rect,
        CornerRadius::same(8),
        p.panel,
        Stroke::new(1.0, p.outline),
        egui::StrokeKind::Inside,
    );
    painter.circle_filled(rect.left_center() + vec2(14.0, 0.0), 4.0, p.accent);
    painter.with_clip_rect(rect.shrink(4.0)).galley(
        pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.text,
    );
}

/// A sidebar playlist as a drop target: a song dropped on one of the
/// account's own playlists is added to it.
pub(super) fn playlist_drop_target(
    ui: &mut Ui,
    response: &egui::Response,
    rect: Rect,
    item: &Item,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    let Some(playlist) = &item.editable else {
        return;
    };
    if shared(ui).is_none() {
        return;
    }
    if response.dnd_hover_payload::<SongDrag>().is_some() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(2.0, p.accent),
            egui::StrokeKind::Inside,
        );
    }
    if let Some(dropped) = response.dnd_release_payload::<SongDrag>() {
        actions.push(Action::Account(AccountAction::Add {
            playlist_id: playlist.clone(),
            tracks: vec![dropped.track.clone()],
        }));
    }
}

/// Save to library, Subscribe, and the own playlist's Edit and Delete, in a
/// page header's button row.
pub(super) fn header_actions(ui: &mut Ui, h: &Header, p: &Palette, actions: &mut Vec<Action>) {
    let Some(s) = shared(ui) else { return };
    if let Some(library) = &h.library {
        let saved = s.marks.saved(library);
        let (text, icon) = if saved {
            ("Remove from library", Icon::Check)
        } else {
            ("Save to library", Icon::Plus)
        };
        if pill(ui, text, Some(icon), false, p).clicked() {
            actions.push(Action::Account(AccountAction::Save {
                playlist_id: library.playlist_id.clone(),
                title: h.title.clone(),
                save: !saved,
            }));
        }
        ui.add_space(8.0);
    }
    if let Some(subscription) = &h.subscription {
        let subscribed = s.marks.subscribed(subscription);
        let response = if subscribed {
            pill(ui, "Subscribed", Some(Icon::Check), false, p).on_hover_text("Unsubscribe")
        } else {
            pill(ui, "Subscribe", None, true, p)
        };
        if response.clicked() {
            actions.push(Action::Account(AccountAction::Subscribe {
                channel_id: subscription.channel_id.clone(),
                name: h.title.clone(),
                subscribe: !subscribed,
            }));
        }
        ui.add_space(8.0);
    }
    if let Some(playlist_id) = &h.editable {
        if pill(ui, "Edit playlist", Some(Icon::Pencil), false, p).clicked() {
            actions.push(Action::Account(AccountAction::Dialog(Some(
                Dialog::EditPlaylist {
                    playlist_id: playlist_id.clone(),
                    title: h.title.clone(),
                    description: h.description.clone().unwrap_or_default(),
                },
            ))));
        }
        ui.add_space(8.0);
        if pill(ui, "Delete playlist", Some(Icon::Trash), false, p).clicked() {
            actions.push(Action::Account(AccountAction::Dialog(Some(
                Dialog::DeletePlaylist {
                    playlist_id: playlist_id.clone(),
                    title: h.title.clone(),
                },
            ))));
        }
    }
}

/// Library → Playlists: New playlist, after the chips.
pub(super) fn library_actions(
    ui: &mut Ui,
    tab: LibraryTab,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    if tab != LibraryTab::Playlists || shared(ui).is_none() {
        return;
    }
    ui.add_space(12.0);
    if pill(ui, "New playlist", Some(Icon::Plus), true, p).clicked() {
        actions.push(Action::Account(AccountAction::Dialog(Some(
            Dialog::NewPlaylist {
                title: String::new(),
                description: String::new(),
                tracks: Vec::new(),
            },
        ))));
    }
}

/// A labelled single-line field, named for screen readers and the E2E
/// driver. Its text is selected when it takes focus, so typing replaces it.
fn field(ui: &mut Ui, name: &str, text: &mut String, p: &Palette, focus: bool) -> egui::Response {
    let caption = ui.label(
        RichText::new(name)
            .font(font(Weight::Medium, 13.0))
            .color(p.secondary),
    );
    let mut output = egui::TextEdit::singleline(text)
        .font(font(Weight::Regular, 15.0))
        .desired_width(f32::INFINITY)
        .margin(vec2(10.0, 8.0))
        .show(ui);
    let response = egui::Response::clone(&output.response).labelled_by(caption.id);
    if focus {
        response.request_focus();
    }
    if focus || response.gained_focus() {
        let all = egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(text.chars().count()),
        );
        output.state.cursor.set_char_range(Some(all));
        output.state.store(ui.ctx(), response.id);
    }
    #[cfg(feature = "e2e")]
    crate::e2e::register(&response.ctx, name, response.interact_rect);
    ui.add_space(10.0);
    response
}

/// The open dialog, if any: new playlist, edit or delete a playlist, or the
/// Add to playlist picker.
pub(super) fn dialogs(app: &mut App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let Some(mut dialog) = app.account_state.dialog.take() else {
        return;
    };
    let own = app.own_playlists();
    let ctx = ui.ctx().clone();
    // The first frame of a dialog focuses its first field.
    let first = ctx.data_mut(|d| {
        let id = Id::new("ytfast-dialog-open");
        let fresh = !d.get_temp::<bool>(id).unwrap_or(false);
        d.insert_temp(id, true);
        fresh
    });
    let mut keep = true;
    let modal = egui::Modal::new(Id::new("ytfast-account-dialog"))
        .frame(
            egui::Frame::new()
                .fill(p.panel)
                .stroke(Stroke::new(1.0, p.outline))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(egui::Margin::same(24)),
        )
        .backdrop_color(p.overlay.gamma_multiply(0.6))
        .show(&ctx, |ui| {
            ui.set_width(420.0);
            match &mut dialog {
                Dialog::NewPlaylist {
                    title,
                    description,
                    tracks,
                } => {
                    label(ui, "New playlist", 20.0, Weight::Bold, p.text);
                    ui.add_space(16.0);
                    let name = field(ui, "Playlist name", title, p, first);
                    field(ui, "Description", description, p, false);
                    let enter = name.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let (cancel, done) = buttons(ui, "Create", !title.trim().is_empty(), p);
                    if (done || enter) && !title.trim().is_empty() {
                        actions.push(Action::Account(AccountAction::Create {
                            title: title.clone(),
                            description: description.clone(),
                            tracks: std::mem::take(tracks),
                        }));
                        keep = false;
                    }
                    keep &= !cancel;
                }
                Dialog::EditPlaylist {
                    playlist_id,
                    title,
                    description,
                } => {
                    label(ui, "Edit playlist", 20.0, Weight::Bold, p.text);
                    ui.add_space(16.0);
                    field(ui, "Playlist name", title, p, first);
                    field(ui, "Description", description, p, false);
                    let (cancel, done) = buttons(ui, "Save", !title.trim().is_empty(), p);
                    if done {
                        actions.push(Action::Account(AccountAction::Details {
                            playlist_id: playlist_id.clone(),
                            title: title.clone(),
                            description: description.clone(),
                        }));
                        keep = false;
                    }
                    keep &= !cancel;
                }
                Dialog::DeletePlaylist { playlist_id, title } => {
                    label(ui, "Delete playlist", 20.0, Weight::Bold, p.text);
                    ui.add_space(12.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!(
                                "“{title}” will be deleted from your account. This can't be undone."
                            ))
                            .font(font(Weight::Regular, 14.5))
                            .color(p.secondary),
                        )
                        .wrap(),
                    );
                    ui.add_space(16.0);
                    let (cancel, done) = buttons(ui, "Delete", true, p);
                    if done {
                        actions.push(Action::Account(AccountAction::Delete {
                            playlist_id: playlist_id.clone(),
                        }));
                        keep = false;
                    }
                    keep &= !cancel;
                }
                Dialog::AddToPlaylist {
                    tracks,
                    filter,
                    selected,
                } => {
                    let what = match tracks.as_slice() {
                        [one] => format!("Add “{}” to", one.title),
                        many => format!("Add {} songs to", many.len()),
                    };
                    label(ui, what, 18.0, Weight::Bold, p.text);
                    ui.add_space(12.0);
                    let needle = filter.trim().to_lowercase();
                    let matches: Vec<&(String, String)> = own
                        .iter()
                        .filter(|(_, t)| needle.is_empty() || t.to_lowercase().contains(&needle))
                        .collect();
                    let (down, up, enter) = ui.input(|i| {
                        (
                            i.key_pressed(Key::ArrowDown),
                            i.key_pressed(Key::ArrowUp),
                            i.key_pressed(Key::Enter),
                        )
                    });
                    let search = field(ui, "Find a playlist", filter, p, first);
                    if search.changed() {
                        *selected = 0;
                    }
                    if down {
                        *selected = (*selected + 1).min(matches.len().saturating_sub(1));
                    }
                    if up {
                        *selected = selected.saturating_sub(1);
                    }
                    *selected = (*selected).min(matches.len().saturating_sub(1));
                    let mut chosen = (enter && !matches.is_empty()).then_some(*selected);
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            if matches.is_empty() {
                                label(
                                    ui,
                                    if own.is_empty() {
                                        "You have no playlists yet."
                                    } else {
                                        "No playlist has that name."
                                    },
                                    14.0,
                                    Weight::Regular,
                                    p.secondary,
                                );
                            }
                            for (i, (_, title)) in matches.iter().enumerate() {
                                let (rect, response) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), 44.0),
                                    Sense::click(),
                                );
                                let fill = if i == *selected {
                                    p.surface_active
                                } else if response.hovered() {
                                    p.surface_hover
                                } else {
                                    Color32::TRANSPARENT
                                };
                                ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
                                Icon::Queue.image(p.secondary, 18.0).paint_at(
                                    ui,
                                    Rect::from_min_size(
                                        pos2(rect.left() + 12.0, rect.center().y - 9.0),
                                        Vec2::splat(18.0),
                                    ),
                                );
                                ui.painter().text(
                                    pos2(rect.left() + 42.0, rect.center().y),
                                    Align2::LEFT_CENTER,
                                    title,
                                    font(Weight::Medium, 15.0),
                                    p.text,
                                );
                                if i == *selected && (down || up) {
                                    ui.scroll_to_rect(rect, None);
                                }
                                describe(&response, title, None);
                                if response
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .clicked()
                                {
                                    chosen = Some(i);
                                }
                            }
                        });
                    if let Some((playlist_id, _)) = chosen.and_then(|i| matches.get(i)) {
                        actions.push(Action::Account(AccountAction::Add {
                            playlist_id: playlist_id.clone(),
                            tracks: tracks.clone(),
                        }));
                        keep = false;
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if pill(ui, "New playlist", Some(Icon::Plus), false, p).clicked() {
                            actions.push(Action::Account(AccountAction::Dialog(Some(
                                Dialog::NewPlaylist {
                                    title: String::new(),
                                    description: String::new(),
                                    tracks: tracks.clone(),
                                },
                            ))));
                            keep = false;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if pill(ui, "Cancel", None, false, p).clicked() {
                                keep = false;
                            }
                        });
                    });
                }
            }
        });
    if modal.should_close() {
        keep = false;
    }
    if keep && app.account_state.dialog.is_none() {
        app.account_state.dialog = Some(dialog);
    } else {
        ctx.data_mut(|d| d.remove::<bool>(Id::new("ytfast-dialog-open")));
    }
}

/// Cancel and the confirming button, right-aligned; (cancelled, confirmed).
fn buttons(ui: &mut Ui, confirm: &str, enabled: bool, p: &Palette) -> (bool, bool) {
    ui.add_space(6.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let done = ui
            .add_enabled_ui(enabled, |ui| pill(ui, confirm, None, true, p))
            .inner
            .clicked();
        ui.add_space(8.0);
        let cancel = pill(ui, "Cancel", None, false, p).clicked();
        (cancel, done)
    })
    .inner
}
