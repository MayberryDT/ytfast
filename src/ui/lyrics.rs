//! Now Playing's Lyrics tab. Timed lyrics follow the song: the current line
//! large and lit in the cover's accent about a third of the way down,
//! upcoming lines readable, past lines dimmer. Scrolling by hand holds the
//! view for a few seconds; clicking a line seeks to it.

use std::sync::Arc;
use std::time::Duration;

use super::widgets::{font, label, named};
use crate::app::{Action, App};
use crate::backend::Command;
use crate::model::{LyricLine, Lyrics};
use crate::theme::Palette;
use egui::{
    Color32, CornerRadius, FontId, Id, Painter, RichText, ScrollArea, Sense, Ui, pos2, vec2,
};
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

/// A lyric line's text as shown: an instrumental gap is a note.
pub(super) fn line_text(line: &LyricLine) -> &str {
    if line.text.is_empty() {
        "♪"
    } else {
        line.text.as_str()
    }
}

/// Where each lyric line sits when laid out in one font and width: its
/// height and its top (lines `gap` apart). Measured once per song, font
/// and width and kept in the window's memory, so a frame lays out only
/// the lines it shows.
#[derive(Clone)]
pub(super) struct Metrics {
    /// What was measured: song, font, width and gap.
    key: Id,
    pub heights: Arc<[f32]>,
    pub tops: Arc<[f32]>,
    /// Below the last line and its gap.
    pub total: f32,
}

impl Metrics {
    /// The first line that can show with the lines scrolled `offset` up.
    pub fn first_visible(&self, offset: f32) -> usize {
        self.tops
            .partition_point(|&t| t <= offset)
            .saturating_sub(1)
    }
}

/// The metrics of `lyrics` (song `id`) in `font` at `width`, from the
/// window's memory when they were measured before. `slot` names the view
/// asking, which keeps one measurement at a time.
pub(super) fn metrics(
    painter: &Painter,
    lyrics: &Lyrics,
    id: &str,
    font: &FontId,
    width: f32,
    gap: f32,
    slot: (&str, bool),
) -> Metrics {
    let ctx = painter.ctx();
    let key = Id::new(("lyric-metrics", id, font, width.to_bits(), gap.to_bits()));
    let slot = Id::new(("lyric-metrics-slot", slot));
    if let Some(m) = ctx.data(|d| d.get_temp::<Metrics>(slot))
        && m.key == key
    {
        return m;
    }
    let mut heights = Vec::with_capacity(lyrics.lines.len());
    let mut tops = Vec::with_capacity(lyrics.lines.len());
    let mut y = 0.0_f32;
    for line in &lyrics.lines {
        let height = painter
            .layout(
                line_text(line).to_owned(),
                font.clone(),
                Color32::PLACEHOLDER,
                width,
            )
            .size()
            .y;
        tops.push(y);
        heights.push(height);
        y += height + gap;
    }
    let m = Metrics {
        key,
        heights: heights.into(),
        tops: tops.into(),
        total: y,
    };
    ctx.data_mut(|d| d.insert_temp(slot, m.clone()));
    m
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

    // Current large and lit, past dim, upcoming readable. Every line is
    // measured once in the regular style; the lit line, laid out each
    // frame, pushes the lines after it down by its extra height.
    let regular = font(Weight::SemiBold, 21.0);
    let m = metrics(&painter, lyrics, id, &regular, width, GAP, ("tab", true));
    let lit = current.map(|c| {
        let galley = painter.layout(
            line_text(&lyrics.lines[c]).to_owned(),
            font(Weight::Bold, 26.0),
            Color32::PLACEHOLDER,
            width,
        );
        (c, galley)
    });
    let grow = lit
        .as_ref()
        .map_or(0.0, |(c, g)| g.size().y - m.heights[*c]);
    let top_of = |i: usize| {
        m.tops[i]
            + if current.is_some_and(|c| i > c) {
                grow
            } else {
                0.0
            }
    };
    let y = m.total + grow;
    let source = lyrics
        .source
        .as_ref()
        .map(|s| painter.layout(s.clone(), font(Weight::Regular, 12.0), p.dim, width));
    let total = y + source.as_ref().map_or(0.0, |g| g.size().y + 16.0);
    // The last line can still come up to a third of the way down.
    let max_offset = (total - rect.height() * 0.6).max(0.0);
    let target = lit
        .as_ref()
        .map(|(c, g)| top_of(*c) + g.size().y / 2.0 - rect.height() / 3.0)
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

    for i in m.first_visible(follow.offset - grow.max(0.0))..lyrics.lines.len() {
        let top = rect.top() + top_of(i) - follow.offset;
        if top > rect.bottom() {
            break;
        }
        let (galley, color) = match &lit {
            Some((c, galley)) if *c == i => (galley.clone(), p.accent),
            _ => {
                let color = if current.is_some_and(|c| i < c) {
                    p.dim
                } else {
                    p.secondary
                };
                let galley = painter.layout(
                    line_text(&lyrics.lines[i]).to_owned(),
                    regular.clone(),
                    Color32::PLACEHOLDER,
                    width,
                );
                (galley, color)
            }
        };
        let line_rect =
            egui::Rect::from_min_size(pos2(rect.left() + 8.0, top), vec2(width, galley.size().y));
        if !line_rect.intersects(rect) {
            continue;
        }
        let visible = line_rect.expand2(vec2(8.0, GAP / 2.0)).intersect(rect);
        let response = ui.interact(visible, Id::new(("lyric", id, i)), Sense::click());
        if response.hovered() {
            painter.rect_filled(visible, CornerRadius::same(6), p.surface_hover);
        }
        painter.galley(line_rect.min, galley, color);
        if named(response, line_text(&lyrics.lines[i]))
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
