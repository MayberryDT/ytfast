//! The mini player: a small window of its own (app id `ytfast-mini`, about
//! 380×128) with the cover, title, artists, a progress bar you can seek and
//! the transport, suited to floating and pinning in Hyprland. Switching to it
//! closes the full window; the button on the right brings that back.

use super::widgets::{cover, font, icon_button, label, named, named_as, runs_line};
use crate::app::{Action, App};
use crate::backend::Command;
use crate::icons::Icon;
use crate::model::format_time;
use egui::{Align, Id, Layout, Rect, RichText, Sense, Ui, Vec2, pos2, vec2};
use fastframe_fonts::Weight;

/// The mini player window's size, in points.
pub const SIZE: [f32; 2] = [380.0, 128.0];

pub fn draw(app: &mut App, ui: &mut Ui, actions: &mut Vec<Action>) {
    let p = app.palette.clone();
    super::keyboard(app, ui, actions);
    egui::CentralPanel::no_frame().show(ui, |ui| {
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, p.panel);
        let inner = full.shrink(12.0);
        let pb = &app.playback;
        let track = pb.index.and_then(|i| app.queue.get(i));

        // Cover: opens the full window on Now Playing.
        let art = Rect::from_min_size(inner.min, Vec2::splat(inner.height()));
        cover(
            ui,
            art,
            track.and_then(|t| t.thumbnail.as_deref()),
            false,
            6,
            &p,
        );
        let art_response = ui.interact(art, Id::new("mini-cover"), Sense::click());
        if named(art_response, "Cover")
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Open the full player")
            .clicked()
        {
            actions.push(Action::NowPlaying(true));
            actions.push(Action::MiniPlayer(false));
        }

        let right = Rect::from_min_max(pos2(art.right() + 14.0, inner.top()), inner.max);
        let mut column = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(right)
                .layout(Layout::top_down(Align::Min)),
        );
        let column = &mut column;
        column.spacing_mut().item_spacing.y = 2.0;
        match track {
            Some(track) => {
                column.add(
                    egui::Label::new(
                        RichText::new(&track.title)
                            .font(font(Weight::SemiBold, 15.0))
                            .color(p.text),
                    )
                    .truncate()
                    .selectable(false),
                );
                // A linked artist opens their page, in the full window.
                let mut links = Vec::new();
                runs_line(column, &track.artists, 13.0, &p, &mut links);
                if !links.is_empty() {
                    actions.append(&mut links);
                    actions.push(Action::MiniPlayer(false));
                }
            }
            None => {
                label(
                    column,
                    "Nothing playing",
                    15.0,
                    Weight::SemiBold,
                    p.secondary,
                );
            }
        }

        // Progress: seeks on click or drag.
        let duration = pb.duration.max(0.0);
        let bar = Rect::from_min_size(
            pos2(right.left(), right.top() + 44.0),
            vec2(right.width(), 14.0),
        );
        let response = named_as(
            ui.interact(bar, Id::new("mini-seek"), Sense::click_and_drag()),
            egui::WidgetType::Slider,
            "Seek",
        );
        let pointer = response
            .interact_pointer_pos()
            .map(|pos| ((pos.x - bar.left()) / bar.width()).clamp(0.0, 1.0));
        let shown = match (response.dragged(), pointer) {
            (true, Some(f)) => f64::from(f) * duration,
            _ => pb.position,
        };
        if (response.drag_stopped() || response.clicked())
            && duration > 0.0
            && let Some(f) = pointer
        {
            actions.push(Action::Command(Command::Seek(f64::from(f) * duration)));
        }
        let active = response.hovered() || response.dragged();
        let thickness = if active { 5.0 } else { 3.0 };
        let line = Rect::from_center_size(bar.center(), vec2(bar.width(), thickness));
        ui.painter().rect_filled(line, 2.0, p.surface_active);
        let fraction = if duration > 0.0 {
            (shown / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        ui.painter().rect_filled(
            Rect::from_min_size(line.min, vec2(line.width() * fraction, thickness)),
            2.0,
            p.accent,
        );
        if active {
            ui.painter().circle_filled(
                pos2(line.left() + line.width() * fraction, line.center().y),
                6.0,
                p.accent,
            );
        }

        // Transport, the time, and the way back to the full window.
        let controls = Rect::from_min_max(pos2(right.left() - 8.0, bar.bottom() + 4.0), right.max);
        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(controls)
                .layout(Layout::left_to_right(Align::Center)),
        );
        let row = &mut row;
        row.spacing_mut().item_spacing.x = 0.0;
        if icon_button(row, Icon::SkipBack, 18.0, p.text, &p, "Previous").clicked() {
            actions.push(Action::Command(Command::Previous));
        }
        let (icon, tip) = if pb.playing {
            (Icon::Pause, "Pause")
        } else {
            (Icon::Play, "Play")
        };
        if icon_button(row, icon, 22.0, p.text, &p, tip).clicked() {
            actions.push(Action::Command(Command::TogglePause));
        }
        if icon_button(row, Icon::SkipForward, 18.0, p.text, &p, "Next").clicked() {
            actions.push(Action::Command(Command::Next));
        }
        row.add_space(8.0);
        label(
            row,
            format!("{} / {}", format_time(shown), format_time(duration)),
            12.0,
            Weight::Regular,
            p.secondary,
        );
        row.with_layout(Layout::right_to_left(Align::Center), |row| {
            if icon_button(row, Icon::FullPlayer, 18.0, p.secondary, &p, "Full player").clicked() {
                actions.push(Action::MiniPlayer(false));
            }
        });
    });
}
