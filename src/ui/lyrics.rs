//! Now Playing's Lyrics tab. Timed lyrics follow the song: the current line
//! large and lit in the cover's accent about a third of the way down,
//! upcoming lines readable, past lines dimmer. Scrolling by hand holds the
//! view for a few seconds; clicking a line seeks to it.

use std::time::Duration;

use super::widgets::{font, label, named};
use crate::app::{Action, App};
use crate::backend::Command;
use crate::model::Lyrics;
use crate::theme::Palette;
use egui::{CornerRadius, Id, RichText, ScrollArea, Sense, Ui, pos2, vec2};
use fastframe_fonts::Weight;

/// How long a hand scroll holds the view before it follows the song again.
const HOLD: f64 = 4.0;
/// Space between lines.
const GAP: f32 = 14.0;

pub(super) fn lyrics(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let none = |ui: &mut Ui| {
        label(
            ui,
            "Lyrics aren't available for this song.",
            15.0,
            Weight::Regular,
            p.secondary,
        );
    };
    let Some(track) = app.current_track() else {
        none(ui);
        return;
    };
    let id = &track.video_id;
    match app.lyrics.get(id) {
        None => {
            actions.push(Action::Lyrics(id.clone()));
            ui.add(egui::Spinner::new().size(20.0).color(p.secondary));
        }
        Some(Err(error)) => {
            ui.horizontal(|ui| {
                label(
                    ui,
                    format!("Couldn't load the lyrics. {error}"),
                    14.0,
                    Weight::Regular,
                    p.secondary,
                );
                if ui.link("Retry").clicked() {
                    actions.push(Action::RetryLyrics(id.clone()));
                }
            });
        }
        Some(Ok(None)) => none(ui),
        Some(Ok(Some(lyrics))) if !lyrics.lines.is_empty() => {
            timed(app, ui, lyrics, id, p, actions);
        }
        Some(Ok(Some(lyrics))) => plain(ui, lyrics, id, p),
    }
}

fn plain(ui: &mut Ui, lyrics: &Lyrics, id: &str, p: &Palette) {
    ScrollArea::vertical()
        .id_salt(("lyrics", id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(&lyrics.text)
                        .font(font(Weight::Medium, 17.0))
                        .color(p.text),
                )
                .wrap(),
            );
            if let Some(source) = &lyrics.source {
                ui.add_space(16.0);
                label(ui, source, 12.0, Weight::Regular, p.dim);
            }
        });
}

/// Where the timed view is scrolled to, kept between frames.
#[derive(Clone, Copy, Default)]
struct Follow {
    offset: f32,
    /// Until when a hand scroll holds the view (egui time, seconds).
    held_until: f64,
    /// The previous frame's time, for the easing.
    last: f64,
    started: bool,
}

fn timed(
    app: &App,
    ui: &mut Ui,
    lyrics: &Lyrics,
    id: &str,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    let (rect, area) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
    let position = app.position_now();
    let current = crate::lyrics::current_line(&lyrics.lines, position);
    let width = (rect.width() - 16.0).max(80.0);
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));

    // Lay the lines out: current large and lit, past dim, upcoming readable.
    let mut lines = Vec::with_capacity(lyrics.lines.len());
    let mut y = 0.0_f32;
    for (i, line) in lyrics.lines.iter().enumerate() {
        let (size, weight, color) = match current {
            Some(c) if c == i => (26.0, Weight::Bold, p.accent),
            Some(c) if i < c => (21.0, Weight::SemiBold, p.dim),
            _ => (21.0, Weight::SemiBold, p.secondary),
        };
        let text = if line.text.is_empty() {
            "♪"
        } else {
            line.text.as_str()
        };
        let galley = painter.layout(text.to_owned(), font(weight, size), color, width);
        let height = galley.size().y;
        lines.push((y, galley));
        y += height + GAP;
    }
    let source = lyrics
        .source
        .as_ref()
        .map(|s| painter.layout(s.clone(), font(Weight::Regular, 12.0), p.dim, width));
    let total = y + source.as_ref().map_or(0.0, |g| g.size().y + 16.0);
    // The last line can still come up to a third of the way down.
    let max_offset = (total - rect.height() * 0.6).max(0.0);
    let target = current
        .map(|c| lines[c].0 + lines[c].1.size().y / 2.0 - rect.height() / 3.0)
        .unwrap_or(0.0)
        .clamp(0.0, max_offset);

    let now = ui.input(|i| i.time);
    let state_id = Id::new(("lyrics-follow", id));
    let mut follow: Follow = ui.data(|d| d.get_temp(state_id)).unwrap_or_default();
    if !follow.started {
        follow = Follow {
            offset: target,
            started: true,
            last: now,
            held_until: 0.0,
        };
    }
    if area.contains_pointer() {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        if dy != 0.0 {
            follow.offset = (follow.offset - dy).clamp(0.0, max_offset);
            follow.held_until = now + HOLD;
        }
    }
    let held = now < follow.held_until;
    if !held {
        let dt = (now - follow.last).clamp(0.0, 0.1) as f32;
        follow.offset += (target - follow.offset) * (1.0 - (-dt * 9.0).exp());
        if (target - follow.offset).abs() < 0.5 {
            follow.offset = target;
        }
    }
    follow.last = now;

    for (i, (top, galley)) in lines.iter().enumerate() {
        let line_rect = egui::Rect::from_min_size(
            pos2(rect.left() + 8.0, rect.top() + top - follow.offset),
            vec2(width, galley.size().y),
        );
        if !line_rect.intersects(rect) {
            continue;
        }
        let visible = line_rect.expand2(vec2(8.0, GAP / 2.0)).intersect(rect);
        let response = ui.interact(visible, Id::new(("lyric", id, i)), Sense::click());
        if response.hovered() {
            painter.rect_filled(visible, CornerRadius::same(6), p.surface_hover);
        }
        painter.galley(line_rect.min, galley.clone(), p.secondary);
        if named(response, galley.text())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            actions.push(Action::Command(Command::Seek(lyrics.lines[i].start)));
            follow.held_until = 0.0;
        }
    }
    if let Some(source) = source {
        let top = rect.top() + y + 16.0 - follow.offset;
        if top < rect.bottom() {
            painter.galley(pos2(rect.left() + 8.0, top), source, p.dim);
        }
    }
    ui.data_mut(|d| d.insert_temp(state_id, follow));

    // Draw again when something will move: the scroll easing, the end of a
    // hold, or the next line's start.
    let ctx = ui.ctx();
    if !held && (target - follow.offset).abs() > 0.0 {
        ctx.request_repaint();
    } else if held {
        ctx.request_repaint_after(Duration::from_secs_f64(follow.held_until - now));
    }
    if app.playback.playing {
        let next = current.map_or(0, |c| c + 1);
        if let Some(line) = lyrics.lines.get(next) {
            let wait = (line.start - position).max(0.0);
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
        }
    }
}
