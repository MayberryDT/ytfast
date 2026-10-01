use super::PLAYER;
use super::motion;
use super::widgets::{
    cover, font, icon_button, label, landing_cover, named, named_as, runs_line, track_line,
};
use crate::app::{Action, App};
use crate::backend::Command;
use crate::icons::Icon;
use crate::model::{Repeat, format_time};
use crate::theme::Palette;
use egui::{Align, Id, Layout, Rect, RichText, Sense, Ui, Vec2, pos2, vec2};
use fastframe_fonts::Weight;

pub(super) fn player_bar(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let pb = &app.playback;
    let full = ui.max_rect();
    // Progress along the top edge; it thickens on hover and seeks on click or drag.
    let bar = Rect::from_min_size(full.min, vec2(full.width(), 12.0));
    let response = named_as(
        ui.interact(bar, Id::new("seek"), Sense::click_and_drag()),
        egui::WidgetType::Slider,
        "Seek",
    );
    let active = response.hovered() || response.dragged();
    let duration = pb.duration.max(0.0);
    let pointer_fraction = response
        .interact_pointer_pos()
        .map(|pos| ((pos.x - bar.left()) / bar.width()).clamp(0.0, 1.0));
    let shown = match (response.dragged(), pointer_fraction) {
        (true, Some(f)) => f64::from(f) * duration,
        _ => pb.position,
    };
    if (response.drag_stopped() || response.clicked())
        && duration > 0.0
        && let Some(f) = pointer_fraction
    {
        actions.push(Action::Command(Command::Seek(f64::from(f) * duration)));
    }
    // The bar thickens and the handle grows with weight, not in a snap.
    let hover = motion::spring(
        ui.ctx(),
        Id::new("seek-hover"),
        if active { 1.0 } else { 0.0 },
        380.0,
    );
    let thickness = 2.0 + 2.0 * hover;
    let track_rect = Rect::from_min_size(full.min, vec2(full.width(), thickness));
    ui.painter().rect_filled(track_rect, 0.0, p.surface_active);
    let target = if duration > 0.0 {
        (shown / duration).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    // Playback creeps forward exactly; a seek (a jump) travels there.
    let drawn_key = Id::new("seek-drawn");
    let drawn: f32 = ui.data(|d| d.get_temp(drawn_key)).unwrap_or(target);
    let fraction = if response.dragged() || (target - drawn).abs() * full.width() < 6.0 {
        target
    } else {
        motion::drive(
            ui.ctx(),
            Id::new("seek-travel"),
            drawn * full.width(),
            target * full.width(),
            300.0,
        ) / full.width()
    };
    ui.data_mut(|d| d.insert_temp(drawn_key, fraction));
    ui.painter().rect_filled(
        Rect::from_min_size(full.min, vec2(full.width() * fraction, thickness)),
        0.0,
        p.accent,
    );
    let knob = motion::spring(
        ui.ctx(),
        Id::new("seek-knob"),
        if response.dragged() {
            1.25
        } else if active {
            1.0
        } else {
            0.0
        },
        520.0,
    );
    if knob > 0.02 {
        ui.painter().circle_filled(
            pos2(
                full.left() + full.width() * fraction,
                full.top() + thickness / 2.0,
            ),
            6.0 * knob,
            p.accent,
        );
    }
    if active && let Some(pos) = response.hover_pos() {
        let at = f64::from(((pos.x - bar.left()) / bar.width()).clamp(0.0, 1.0)) * duration;
        response.clone().on_hover_text_at_pointer(format_time(at));
    }

    let inner = full.shrink2(vec2(16.0, 0.0)).with_min_y(full.top() + 4.0);
    let mut ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let ui = &mut ui;
    // Transport.
    if icon_button(ui, Icon::SkipBack, 22.0, p.text, p, "Previous").clicked() {
        actions.push(Action::Command(Command::Previous));
    }
    let (rect, play) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::click());
    // The button gives under the press and swells a little under the pointer.
    let press = motion::spring(
        ui.ctx(),
        play.id.with("press"),
        if play.is_pointer_button_down_on() {
            0.88
        } else if play.hovered() {
            1.06
        } else {
            1.0
        },
        600.0,
    );
    let hover_disc = motion::lift(ui, play.id, play.hovered());
    if hover_disc > 0.01 {
        ui.painter().circle_filled(
            rect.center(),
            24.0 * press,
            p.surface_hover.gamma_multiply(hover_disc),
        );
    }
    if pb.loading {
        egui::Spinner::new()
            .size(28.0)
            .color(p.text)
            .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(28.0)));
    } else {
        // Play and pause are one shape: the triangle's halves become the bars.
        let m = motion::spring(
            ui.ctx(),
            play.id.with("morph"),
            if pb.playing { 1.0 } else { 0.0 },
            420.0,
        );
        play_pause_glyph(ui, rect.center(), 15.0 * press, m, p.text);
    }
    if named(play, if pb.playing { "Pause" } else { "Play" })
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if pb.playing { "Pause" } else { "Play" })
        .clicked()
    {
        actions.push(Action::Command(Command::TogglePause));
    }
    if icon_button(ui, Icon::SkipForward, 22.0, p.text, p, "Next").clicked() {
        actions.push(Action::Command(Command::Next));
    }
    ui.add_space(8.0);
    // A fixed slot, so the song beside it doesn't shift as the digits change.
    let (time, _) = ui.allocate_exact_size(vec2(104.0, 20.0), Sense::hover());
    ui.painter().text(
        time.left_center(),
        egui::Align2::LEFT_CENTER,
        format!("{} / {}", format_time(shown), format_time(duration)),
        font(Weight::Regular, 13.0),
        p.secondary,
    );

    // Right side first, so the middle gets what is left.
    let right_width = 340.0;
    let middle_width = (ui.available_width() - right_width).max(160.0);
    let (middle, _) = ui.allocate_exact_size(vec2(middle_width, PLAYER - 8.0), Sense::hover());
    let current = pb.index.and_then(|i| app.queue.get(i));
    if let Some(track) = current {
        let mut mid = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(middle.shrink2(vec2(24.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        mid.set_clip_rect(middle.intersect(mid.clip_rect()));
        let (art, art_response) = mid.allocate_exact_size(Vec2::splat(48.0), Sense::click());
        // The handoff: at a song change the new cover and title roll in from
        // below while the old ones leave upwards.
        let handoff = motion::changed(mid.ctx(), Id::new("player-handoff"), &track.video_id, 0.5)
            .map(|(previous, t)| (previous, motion::ease_out(t)));
        let shift = handoff.as_ref().map_or(0.0, |(_, k)| 1.0 - k);
        let previous = handoff
            .as_ref()
            .and_then(|(id, _)| app.queue.iter().find(|t| &t.video_id == id));
        if let (Some(old), Some((_, k))) = (previous, &handoff) {
            let mut fading =
                mid.new_child(egui::UiBuilder::new().max_rect(motion::offset(art, 0.0, -0.9 * k)));
            fading.multiply_opacity(1.0 - k);
            cover(
                &mut fading,
                motion::offset(art, 0.0, -0.9 * k),
                old.thumbnail.as_deref(),
                false,
                4,
                p,
            );
        }
        let art_now = motion::offset(art, 0.0, 0.9 * shift);
        landing_cover(
            &mut mid,
            motion::player_site(),
            art_now,
            track.thumbnail.as_deref(),
            false,
            4,
            p,
        );
        if let Some(url) = &track.thumbnail {
            motion::origin(mid.ctx(), "player", url, art, 4.0);
        }
        if named(art_response, "Cover")
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            actions.push(Action::NowPlaying(!app.now_playing));
        }
        mid.add_space(12.0);
        let text_area = Rect::from_min_max(
            pos2(mid.cursor().left(), middle.top()),
            pos2(middle.right() - 24.0, middle.bottom()),
        );
        if let (Some(old), Some((_, k))) = (previous, &handoff) {
            let mut leaving = mid.new_child(
                egui::UiBuilder::new()
                    .max_rect(text_area.translate(vec2(0.0, -18.0 * k)))
                    .layout(Layout::top_down(Align::Min)),
            );
            leaving.multiply_opacity(1.0 - k);
            leaving.add_space(18.0);
            label(&mut leaving, &old.title, 15.0, Weight::SemiBold, p.text);
        }
        let mut block = mid.new_child(
            egui::UiBuilder::new()
                .max_rect(text_area.translate(vec2(0.0, 18.0 * shift)))
                .layout(Layout::top_down(Align::Min)),
        );
        block.multiply_opacity(1.0 - shift);
        let ui = &mut block;
        {
            ui.add_space(18.0);
            let title = ui.add(
                egui::Label::new(
                    RichText::new(&track.title)
                        .font(font(Weight::SemiBold, 15.0))
                        .color(p.text),
                )
                .truncate()
                .sense(Sense::click())
                .selectable(false),
            );
            if title
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                actions.push(Action::NowPlaying(!app.now_playing));
            }
            runs_line(ui, &track_line(track), 13.5, p, actions);
        }
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let chevron = if app.now_playing {
            Icon::ChevronDown
        } else {
            Icon::ChevronUp
        };
        if icon_button(
            ui,
            chevron,
            22.0,
            p.text,
            p,
            if app.now_playing {
                "Close player"
            } else {
                "Open player"
            },
        )
        .clicked()
        {
            actions.push(Action::NowPlaying(!app.now_playing));
        }
        if icon_button(ui, Icon::MiniPlayer, 20.0, p.secondary, p, "Mini player").clicked() {
            actions.push(Action::MiniPlayer(true));
        }
        let shuffle_color = if pb.shuffle { p.accent } else { p.secondary };
        if icon_button(
            ui,
            Icon::Shuffle,
            20.0,
            shuffle_color,
            p,
            if pb.shuffle {
                "Shuffle on"
            } else {
                "Shuffle off"
            },
        )
        .clicked()
        {
            actions.push(Action::Command(Command::ToggleShuffle));
        }
        let (icon, color, tip) = match pb.repeat {
            Repeat::Off => (Icon::Repeat, p.secondary, "Repeat off"),
            Repeat::All => (Icon::Repeat, p.accent, "Repeat all"),
            Repeat::One => (Icon::RepeatOne, p.accent, "Repeat one"),
        };
        if icon_button(ui, icon, 20.0, color, p, tip).clicked() {
            actions.push(Action::Command(Command::CycleRepeat));
        }
        let mut volume = pb.volume as f32;
        let slider = ui.add_sized(
            vec2(96.0, 20.0),
            egui::Slider::new(&mut volume, 0.0..=100.0)
                .show_value(false)
                .trailing_fill(true),
        );
        if slider.changed() {
            actions.push(Action::Command(Command::Volume(f64::from(volume))));
        }
        let icon = if pb.volume <= 0.0 {
            Icon::Mute
        } else {
            Icon::Volume
        };
        let mute = icon_button(ui, icon, 20.0, p.secondary, p, "Mute");
        if mute.clicked() {
            actions.push(Action::Command(Command::Volume(if pb.volume > 0.0 {
                0.0
            } else {
                70.0
            })));
        }
        // The wheel over the volume controls turns the volume.
        if ui.rect_contains_pointer(slider.rect.union(mute.rect)) {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                let next = (pb.volume + f64::from(wheel) / 10.0).clamp(0.0, 100.0);
                actions.push(Action::Command(Command::Volume(next)));
            }
        }
    });
}

/// Play (`m` = 0) morphing into pause (`m` = 1), drawn at `center` in a box
/// of half-size `r`: the triangle's left and right halves become the bars.
fn play_pause_glyph(ui: &Ui, center: egui::Pos2, r: f32, m: f32, color: egui::Color32) {
    let at = |x: f32, y: f32| center + vec2(x * r, y * r);
    let lerp = |a: egui::Pos2, b: egui::Pos2| a + (b - a) * m;
    // The triangle, nudged right so it looks centred.
    let (x0, x1, half) = (-0.62, 0.78, 0.72);
    let xm = (x0 + x1) / 2.0;
    let top = |x: f32| -half + (x - x0) / (x1 - x0) * half;
    let play_left = [
        at(x0, -half),
        at(xm, top(xm)),
        at(xm, -top(xm)),
        at(x0, half),
    ];
    // The right half starts a hair inside the left one: no antialiased seam.
    let xr = xm - 0.06;
    let play_right = [at(xr, top(xr)), at(x1, 0.0), at(x1, 0.0), at(xr, -top(xr))];
    let (bar, gap, h) = (0.36, 0.2, 0.64);
    let pause_left = [
        at(-gap - bar, -h),
        at(-gap, -h),
        at(-gap, h),
        at(-gap - bar, h),
    ];
    let pause_right = [at(gap, -h), at(gap + bar, -h), at(gap + bar, h), at(gap, h)];
    for (from, to) in [(play_left, pause_left), (play_right, pause_right)] {
        let points: Vec<egui::Pos2> = from
            .iter()
            .zip(to.iter())
            .map(|(a, b)| lerp(*a, *b))
            .collect();
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            color,
            egui::Stroke::NONE,
        ));
    }
}
