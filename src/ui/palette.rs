//! Play anything (Ctrl+K): one field over a dimmed window. Results come in
//! as you type; the highlighted one is prepared so Enter starts at once.
//! Rows slide in when they first show and glide when the order changes,
//! each on its own spring, so results arriving never make the list jump.

use std::collections::HashMap;

use super::widgets::{cover, font, label, named};
use crate::app::{Action, App};
use crate::control::ControlAction;
use crate::icons::Icon;
use crate::palette::{Chosen, Go, Kind};
use crate::theme::Palette;
use egui::{
    Context, CornerRadius, Event, Id, Key, LayerId, Order, Rect, RichText, Sense, Stroke, Vec2,
    pos2, vec2,
};
use fastframe_fonts::Weight;

const ROW: f32 = 54.0;

fn field_id() -> Id {
    Id::new("ytfast-play-anything-field")
}

/// Where each row is drawn and how far it has come in, by result.
#[derive(Clone, Default)]
struct Rows(HashMap<String, (f32, f32)>);

fn rows_id() -> Id {
    Id::new("ytfast-play-anything-rows")
}

pub(super) fn palette(app: &mut App, ctx: &Context, p: &Palette, actions: &mut Vec<Action>) {
    if !app.control.play_anything.open {
        if ctx.data(|d| d.get_temp::<Rows>(rows_id())).is_some() {
            ctx.data_mut(|d| d.remove::<Rows>(rows_id()));
            ctx.memory_mut(|m| m.surrender_focus(field_id()));
        }
        super::motion::spring(ctx, field_id().with("open"), 0.0, 900.0);
        return;
    }
    let shown = super::motion::spring(ctx, field_id().with("open"), 1.0, 900.0).clamp(0.0, 1.0);

    // The list's keys, before the field can take them; Enter carries Shift.
    let (down, up, enter) = ctx.input_mut(|i| {
        let mut take = |key| {
            let mut found = None;
            i.events.retain(|e| match e {
                Event::Key {
                    key: k,
                    pressed: true,
                    modifiers,
                    ..
                } if *k == key => {
                    found = Some(*modifiers);
                    false
                }
                _ => true,
            });
            found
        };
        (
            take(Key::ArrowDown).is_some(),
            take(Key::ArrowUp).is_some(),
            take(Key::Enter),
        )
    });

    let screen = ctx.content_rect();
    let dim = egui::Area::new(field_id().with("dim"))
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .fade_in(false)
        .show(ctx, |ui| {
            ui.painter()
                .rect_filled(screen, 0.0, p.overlay.gamma_multiply(0.6 * shown));
            ui.allocate_rect(screen, Sense::click())
        });
    // A click on the dimmed window (not on the palette) closes it.
    let panel: Option<Rect> = ctx.data(|d| d.get_temp(field_id().with("rect")));
    let at = ctx.input(|i| i.pointer.interact_pos());
    if dim.inner.clicked() && !panel.zip(at).is_some_and(|(r, pos)| r.contains(pos)) {
        actions.push(Action::Control(ControlAction::PlayAnything(false)));
    }

    let width = 660.0_f32.min(screen.width() - 48.0);
    let top = screen.top() + (screen.height() * 0.14).max(48.0);
    // It drops into place from a little above as it opens.
    let origin = pos2(screen.center().x - width / 2.0, top - 12.0 * (1.0 - shown));
    let area_id = field_id().with("panel");
    ctx.move_to_top(LayerId::new(Order::Foreground, area_id));
    let mut chosen: Option<(usize, bool)> = None;
    let drawn = egui::Area::new(area_id)
        .order(Order::Foreground)
        .fixed_pos(origin)
        .fade_in(false)
        .show(ctx, |ui| {
            ui.multiply_opacity(shown);
            egui::Frame::new()
                .fill(p.panel)
                .stroke(Stroke::new(1.0, p.outline))
                .corner_radius(CornerRadius::same(14))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 16],
                    blur: 48,
                    spread: 0,
                    color: p.shadow,
                })
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(width - 20.0);
                    // The field.
                    let pa = &mut app.control.play_anything;
                    let response = ui
                        .horizontal(|ui| {
                            ui.add_space(8.0);
                            let (icon, _) =
                                ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
                            Icon::Search.image(p.secondary, 20.0).paint_at(ui, icon);
                            ui.add_space(6.0);
                            let edit = egui::TextEdit::singleline(&mut pa.query)
                                .id(field_id())
                                .hint_text(
                                    RichText::new("Play anything: songs, albums, artists, commands")
                                        .color(p.dim),
                                )
                                .font(font(Weight::Regular, 19.0))
                                .frame(egui::Frame::NONE)
                                .margin(vec2(4.0, 10.0))
                                .desired_width(ui.available_width() - 34.0);
                            let response = ui.add(edit);
                            if pa.searching() {
                                ui.add(egui::Spinner::new().size(16.0).color(p.secondary));
                            }
                            response
                        })
                        .inner;
                    #[cfg(feature = "e2e")]
                    crate::e2e::register(ui.ctx(), "Play anything", response.interact_rect);
                    // Asked for only when lost: asking again every frame
                    // would reset the field's key filter and cancel IME input.
                    if !response.has_focus() {
                        response.request_focus();
                    }
                    if response.changed() {
                        pa.edited();
                    }

                    // The results for what is in the field now, in this frame.
                    app.refresh_hits();
                    let pa = &mut app.control.play_anything;
                    let count = pa.hits().len();
                    if down && count > 0 {
                        pa.selected = (pa.selected + 1) % count;
                    }
                    if up && count > 0 {
                        pa.selected = (pa.selected + count - 1) % count;
                    }
                    if let (Some(modifiers), true) = (enter, count > 0) {
                        chosen = Some((pa.selected, modifiers.shift));
                    }
                    if count > 0 {
                        ui.add_space(4.0);
                        ui.painter().hline(
                            ui.max_rect().x_range(),
                            ui.cursor().top(),
                            Stroke::new(1.0, p.outline),
                        );
                        ui.add_space(6.0);
                    }
                    let pa = &app.control.play_anything;
                    let list = ui.allocate_exact_size(
                        vec2(ui.available_width(), count as f32 * ROW),
                        Sense::hover(),
                    ).0;
                    let mut rows = ui.ctx().data(|d| d.get_temp::<Rows>(rows_id())).unwrap_or_default();
                    let mut next = Rows::default();
                    for (i, hit) in pa.hits().iter().enumerate() {
                        let target = i as f32 * ROW;
                        // New rows start in place, faded and a little to the right.
                        let (y, appear) = rows.0.remove(&hit.key).unwrap_or((target, 0.0));
                        let key = Id::new(("play-anything-row", &hit.key));
                        let y = super::motion::drive(ui.ctx(), key.with("y"), y, target, 520.0);
                        let appear =
                            super::motion::drive(ui.ctx(), key.with("in"), appear, 100.0, 420.0);
                        next.0.insert(hit.key.clone(), (y, appear));
                        let a = (appear / 100.0).clamp(0.0, 1.0);
                        let rect = Rect::from_min_size(
                            pos2(list.left() + 14.0 * (1.0 - a), list.top() + y),
                            vec2(list.width(), ROW),
                        );
                        let response = ui.interact(rect, key, Sense::click());
                        let selected = pa.selected == i;
                        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                        row.multiply_opacity(a);
                        if selected || response.hovered() {
                            row.painter().rect_filled(
                                rect,
                                CornerRadius::same(8),
                                if selected { p.surface_active } else { p.surface_hover },
                            );
                        }
                        let thumb = Rect::from_min_size(
                            pos2(rect.left() + 8.0, rect.center().y - 20.0),
                            Vec2::splat(40.0),
                        );
                        match (&hit.thumbnail, hit.kind) {
                            (Some(url), kind) => {
                                cover(&mut row, thumb, Some(url), kind == Kind::Artist, 4, p);
                            }
                            (None, kind) => {
                                row.painter()
                                    .rect_filled(thumb, CornerRadius::same(6), p.surface);
                                let icon = match kind {
                                    Kind::Search => Icon::History,
                                    Kind::Command => Icon::Keyboard,
                                    Kind::Artist => Icon::User,
                                    Kind::Album => Icon::Album,
                                    _ => Icon::Music,
                                };
                                icon.image(p.secondary, 20.0).paint_at(
                                    &row,
                                    Rect::from_center_size(thumb.center(), Vec2::splat(20.0)),
                                );
                            }
                        }
                        let tag = match hit.kind {
                            Kind::Song => "Song",
                            Kind::Album => "Album",
                            Kind::Playlist => "Playlist",
                            Kind::Artist => "Artist",
                            Kind::Search => "Search",
                            Kind::Command => "Command",
                        };
                        row.painter().text(
                            pos2(rect.right() - 12.0, rect.center().y),
                            egui::Align2::RIGHT_CENTER,
                            tag,
                            font(Weight::Medium, 12.0),
                            p.dim,
                        );
                        let mut text = row.new_child(egui::UiBuilder::new().max_rect(
                            Rect::from_min_max(
                                pos2(thumb.right() + 14.0, rect.top() + 8.0),
                                pos2(rect.right() - 84.0, rect.bottom() - 4.0),
                            ),
                        ).layout(egui::Layout::top_down(egui::Align::Min)));
                        label(&mut text, &hit.title, 15.0, Weight::Medium, p.text);
                        label(&mut text, &hit.detail, 13.0, Weight::Regular, p.secondary);
                        if named(response, &hit.title)
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            let shift = ui.input(|input| input.modifiers.shift);
                            chosen = Some((i, shift));
                        }
                        if selected && let Go::Song(track) = &hit.go {
                            actions.push(Action::Prepare(track.video_id.clone()));
                        }
                        if selected && (down || up) {
                            ui.scroll_to_rect(rect, None);
                        }
                    }
                    ui.ctx().data_mut(|d| d.insert_temp(rows_id(), next));

                    // A line under the results: what's happening, and the keys.
                    let note = if count == 0 && pa.query.trim().is_empty() {
                        "Type a song, album or artist, or a command such as “sleep 30” or “eq bass”."
                    } else if count == 0 && pa.searching() {
                        "Searching YouTube Music…"
                    } else if pa.failed() {
                        "Couldn't reach YouTube Music. Showing your library."
                    } else if count == 0 {
                        "Nothing matches that."
                    } else {
                        "↑ ↓ to choose · Enter to play · Shift+Enter to open · Esc to close"
                    };
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(10.0);
                        label(ui, note, 12.5, Weight::Regular, p.dim);
                    });
                });
        });
    ctx.data_mut(|d| d.insert_temp(field_id().with("rect"), drawn.response.rect));

    let Some((index, open)) = chosen else { return };
    match app.control.play_anything.choose(app, index, open) {
        Some(Chosen::Actions(chosen)) => {
            actions.extend(chosen);
            actions.push(Action::Control(ControlAction::PlayAnything(false)));
        }
        Some(Chosen::Complete(text)) => {
            let pa = &mut app.control.play_anything;
            pa.query = text;
            pa.edited();
            // The cursor goes to the end, ready for what follows.
            if let Some(mut state) = egui::TextEdit::load_state(ctx, field_id()) {
                let end = egui::text::CCursor::new(pa.query.chars().count());
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(end)));
                state.store(ctx, field_id());
            }
        }
        None => {}
    }
}
