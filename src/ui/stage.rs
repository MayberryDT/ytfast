//! Stage: the music fills the window (`F`, or a double click on Now
//! Playing's cover; `F` or `Esc` leaves, `F11` goes full screen). A huge,
//! sharp cover over a softened field made from the cover itself, timed
//! lyrics in large type beside or below it, and a minimal transport that
//! fades away while the pointer rests. Colours come from the cover (the
//! Now Playing wash), so text stays legible over any theme.

use std::collections::VecDeque;
use std::time::Duration;

use super::motion;
use super::ridge;
use super::widgets::{font, icon_button, named, named_as, runs_text, track_line};
use crate::app::{Action, App};
use crate::backend::Command;
use crate::icons::Icon;
use crate::model::{Lyrics, Track, format_time};
use crate::theme::Palette;
use egui::{Align2, Color32, CornerRadius, Id, Rect, Sense, Ui, UiBuilder, Vec2, pos2, vec2};
use fastframe_fonts::Weight;

/// Chrome fades after the pointer rests this long (seconds).
const IDLE: f32 = 2.5;
/// A song change: covers, field, titles and lyrics trade places.
const HANDOFF: f64 = 0.6;
/// The softened field's crossfade (seconds).
const FIELD_FADE: f64 = 0.7;
/// Frames kept for the frame-time measurement.
const FRAMES: usize = 1200;

/// Stage's state, kept by the app.
#[derive(Default)]
pub struct Stage {
    pub open: bool,
    /// Stage put the window in full screen (F11): leaving Stage ends it.
    pub fullscreen: bool,
    /// How visible the chrome was on the last frame (0 faded, 1 shown).
    pub chrome: f32,
    /// Counts openings, so each opening starts its motion afresh.
    pub opened: u64,
    /// `stable_dt` of the frames drawn while Stage was open, newest last.
    frames: VecDeque<f32>,
}

impl Stage {
    fn record(&mut self, dt: f32) {
        if self.frames.len() == FRAMES {
            self.frames.pop_front();
        }
        self.frames.push_back(dt);
    }

    /// Starts a new frame-time measurement.
    pub fn clear_frames(&mut self) {
        self.frames.clear();
    }

    /// Mean and worst frame time (ms) over the frames since
    /// [`Stage::clear_frames`], and how many frames there were.
    pub fn frame_times(&self) -> (f32, f32, usize) {
        let n = self.frames.len();
        if n == 0 {
            return (0.0, 0.0, 0);
        }
        let sum: f32 = self.frames.iter().sum();
        let worst = self.frames.iter().copied().fold(0.0, f32::max);
        (sum / n as f32 * 1000.0, worst * 1000.0, n)
    }
}

/// Draws Stage over the whole window.
pub(super) fn stage(app: &mut App, ui: &mut Ui, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    app.stage.record(ctx.input(|i| i.stable_dt));
    let chrome = egui::CentralPanel::no_frame()
        .show(ui, |ui| {
            // Stage takes colour from the cover: never theme-painted.
            crate::derived::without_paint(&ctx, || draw(app, ui, actions))
        })
        .inner;
    app.stage.chrome = chrome;
}

/// A larger copy of a YouTube Music cover, for a huge cover that stays sharp.
fn sharp(url: &str) -> Option<String> {
    url.contains("=w544-h544")
        .then(|| url.replacen("=w544-h544", "=w1200-h1200", 1))
}

fn texture(ctx: &egui::Context, uri: &str, size: Vec2) -> Option<egui::TextureId> {
    match egui::Image::new(uri).load_for_size(ctx, size) {
        Ok(egui::load::TexturePoll::Ready { texture }) => Some(texture.id),
        _ => None,
    }
}

/// The part of a square texture that fills `rect` without stretching.
fn cover_uv(rect: Rect) -> Rect {
    let aspect = rect.width() / rect.height().max(1.0);
    if aspect >= 1.0 {
        let h = 1.0 / aspect;
        Rect::from_min_max(pos2(0.0, 0.5 - h / 2.0), pos2(1.0, 0.5 + h / 2.0))
    } else {
        Rect::from_min_max(pos2(0.5 - aspect / 2.0, 0.0), pos2(0.5 + aspect / 2.0, 1.0))
    }
}

#[derive(Clone, Default)]
struct Field {
    shown: Option<String>,
    previous: Option<String>,
    since: f64,
}

/// The softened field: the cover blurred across the window. A new cover's
/// field (or the same cover under the other kind of theme) fades in over
/// the old one once it is made; until then the old one stays.
fn field(ui: &Ui, rect: Rect, wanted: Option<String>) {
    let ctx = ui.ctx();
    let id = Id::new("stage-field");
    let now = ctx.input(|i| i.time);
    let mut f: Field = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    if let Some(w) = wanted
        && f.shown.as_ref() != Some(&w)
        && texture(ctx, &w, rect.size()).is_some()
    {
        f.previous = f.shown.replace(w);
        f.since = now;
    }
    let t = motion::ease_out(((now - f.since) / FIELD_FADE) as f32);
    if t < 1.0 {
        ctx.request_repaint();
    } else {
        f.previous = None;
    }
    let uv = cover_uv(rect);
    let painter = ui.painter();
    if let Some(previous) = &f.previous
        && let Some(texture) = texture(ctx, previous, rect.size())
    {
        painter.image(texture, rect, uv, Color32::WHITE);
    }
    if let Some(shown) = &f.shown
        && let Some(texture) = texture(ctx, shown, rect.size())
    {
        painter.image(texture, rect, uv, Color32::WHITE.gamma_multiply(t));
    }
    ctx.data_mut(|d| d.insert_temp(id, f));
}

/// The cover, huge: a soft shadow, the cover, and its sharper copy once loaded.
fn big_cover(ui: &Ui, rect: Rect, url: Option<&str>, radius: u8, p: &Palette) {
    let corner = CornerRadius::same(radius);
    let shadow = egui::epaint::Shadow {
        offset: [0, (rect.height() * 0.03) as i8],
        blur: (rect.height() * 0.08).min(255.0) as u8,
        spread: 0,
        color: p.shadow,
    };
    ui.painter().add(shadow.as_shape(rect, corner));
    ui.painter().rect_filled(rect, corner, p.surface);
    let Some(url) = url else {
        let s = rect.width() * 0.3;
        Icon::Music
            .image(p.dim, s)
            .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(s)));
        return;
    };
    egui::Image::new(url)
        .corner_radius(corner)
        .show_loading_spinner(false)
        .paint_at(ui, rect);
    if let Some(big) = sharp(url)
        && texture(ui.ctx(), &big, rect.size()).is_some()
    {
        egui::Image::new(big)
            .corner_radius(corner)
            .show_loading_spinner(false)
            .paint_at(ui, rect);
    }
}

enum Words<'a> {
    Timed(&'a Lyrics),
    Plain(&'a Lyrics),
    None,
}

fn words<'a>(app: &'a App, track: &Track, actions: &mut Vec<Action>) -> Words<'a> {
    match app.lyrics.get(&track.video_id) {
        None => {
            if app.current_track().map(|t| &t.video_id) == Some(&track.video_id) {
                actions.push(Action::Lyrics(track.video_id.clone()));
            }
            Words::None
        }
        Some(Ok(Some(l))) if !l.lines.is_empty() => Words::Timed(l),
        Some(Ok(Some(l))) if !l.text.trim().is_empty() => Words::Plain(l),
        _ => Words::None,
    }
}

/// Where the cover and the words go: the words beside the cover on a wide
/// window, below it on a tall one; with none, the cover alone in the middle.
struct Layout {
    cover: Rect,
    words: Rect,
}

fn layout(content: Rect, presence: f32) -> Layout {
    let title = 92.0;
    let alone_side = (content.height() - title)
        .min(content.width() * 0.62)
        .max(120.0);
    let alone = Rect::from_center_size(
        pos2(content.center().x, content.center().y - title / 2.0),
        Vec2::splat(alone_side),
    );
    let wide = content.width() >= content.height() * 1.15;
    let (with, words) = if wide {
        let column = content.width() * 0.46;
        let side = (content.height() - title).min(column * 0.9).max(120.0);
        let cover = Rect::from_center_size(
            pos2(
                content.left() + column / 2.0,
                content.center().y - title / 2.0,
            ),
            Vec2::splat(side),
        );
        let words = Rect::from_min_max(
            pos2(
                content.left() + column + content.width() * 0.04,
                content.top(),
            ),
            content.max,
        );
        (cover, words)
    } else {
        let side = (content.width() * 0.62)
            .min(content.height() * 0.46 - title)
            .max(120.0);
        let cover = Rect::from_center_size(
            pos2(content.center().x, content.top() + side / 2.0),
            Vec2::splat(side),
        );
        let words = Rect::from_min_max(
            pos2(content.left(), cover.bottom() + title + 12.0),
            content.max,
        );
        (cover, words)
    };
    let k = presence.clamp(0.0, 1.0);
    let cover = Rect::from_min_max(
        alone.min + (with.min - alone.min) * k,
        alone.max + (with.max - alone.max) * k,
    );
    Layout { cover, words }
}

fn draw(app: &App, ui: &mut Ui, actions: &mut Vec<Action>) -> f32 {
    let ctx = ui.ctx().clone();
    let rect = ui.max_rect();
    let (wash, moving) = app.cover_fade.wash(app.cover_colors.as_ref(), &app.palette);
    if moving {
        ctx.request_repaint();
    }
    let tinted = wash.palette(&app.palette);
    let p = &tinted;
    wash.paint(ui.painter(), rect);
    let track = app.current_track();
    let url = track.and_then(|t| t.thumbnail.as_deref());
    field(
        ui,
        rect,
        url.map(|u| crate::derived::soft_uri(u, app.palette.dark)),
    );

    let margin = (rect.width().min(rect.height()) * 0.06).max(24.0);
    let transport = 112.0;
    let content = Rect::from_min_max(
        rect.min + vec2(margin, margin),
        pos2(rect.right() - margin, rect.bottom() - transport),
    );
    let current_words = track.map_or(Words::None, |t| words(app, t, actions));
    // While a new song's lyrics are on their way, the layout stays as it is.
    let presence_id = Id::new(("stage-words-presence", app.stage.opened));
    let answered = track.is_some_and(|t| app.lyrics.contains_key(&t.video_id));
    let wanted = if answered || track.is_none() {
        let has_words = !matches!(current_words, Words::None);
        if has_words { 1.0 } else { 0.0 }
    } else {
        ctx.data(|d| d.get_temp::<f32>(presence_id.with("wanted")))
            .unwrap_or(0.0)
    };
    ctx.data_mut(|d| d.insert_temp(presence_id.with("wanted"), wanted));
    let presence = motion::spring(&ctx, presence_id, wanted, 120.0);
    let at = layout(content, presence);
    let side = at.cover.width();
    let radius = (side * 0.018).clamp(6.0, 16.0) as u8;

    // The handoff: the old cover, title and words leave as the new arrive.
    let video_id = track.map_or("", |t| t.video_id.as_str());
    let handoff_id = Id::new(("stage-handoff", app.stage.opened));
    let handoff = motion::changed(&ctx, handoff_id, video_id, HANDOFF)
        .map(|(previous, t)| (previous, motion::ease_out(t)));
    let shift = handoff.as_ref().map_or(0.0, |(_, k)| 1.0 - k);
    let previous = handoff
        .as_ref()
        .and_then(|(id, k)| Some((app.queue.iter().find(|t| &t.video_id == id)?, *k)));
    if let Some((old, k)) = previous {
        let mut leaving = ui.new_child(UiBuilder::new().max_rect(rect));
        leaving.multiply_opacity(1.0 - k);
        big_cover(
            &leaving,
            motion::offset(at.cover, -0.16 * k, 0.0),
            old.thumbnail.as_deref(),
            radius,
            p,
        );
        titles(&leaving, old, at.cover, -14.0 * k, p);
        let old_words = words(app, old, actions);
        words_view(
            &mut leaving,
            app,
            old,
            &old_words,
            motion::offset(at.words, 0.0, -0.04 * k),
            p,
            actions,
            false,
        );
    }
    let mut arriving = ui.new_child(UiBuilder::new().max_rect(rect));
    arriving.multiply_opacity(1.0 - shift);
    let cover_now = motion::offset(at.cover, 0.16 * shift, 0.0);
    // The cover flying in from Now Playing (or back to it) lands here.
    if motion::land(
        &arriving,
        motion::stage_site(),
        url,
        cover_now,
        f32::from(radius),
    ) {
        arriving.painter().rect_filled(
            cover_now,
            CornerRadius::same(radius),
            p.surface.gamma_multiply(0.4),
        );
    } else {
        big_cover(&arriving, cover_now, url, radius, p);
    }
    if let Some(url) = url {
        motion::origin(&ctx, "stage", url, at.cover, f32::from(radius));
    }
    let art = ui.interact(at.cover, Id::new("stage-cover"), Sense::click());
    if named(art, "Stage cover").double_clicked() {
        actions.push(Action::Stage(false));
    }
    if let Some(track) = track {
        titles(&arriving, track, at.cover, 14.0 * shift, p);
        if presence > 0.01 {
            let mut words_ui = arriving.new_child(UiBuilder::new().max_rect(at.words));
            words_ui.multiply_opacity(presence);
            words_view(
                &mut words_ui,
                app,
                track,
                &current_words,
                motion::offset(at.words, 0.0, 0.04 * shift),
                p,
                actions,
                true,
            );
        }
    }

    chrome(app, ui, rect, margin, p, actions)
}

/// Title and artists, centred under the cover.
fn titles(ui: &Ui, track: &Track, cover: Rect, dy: f32, p: &Palette) {
    let size = (cover.width() * 0.055).clamp(20.0, 34.0);
    let painter = ui.painter();
    let width = (cover.width() * 1.3).max(240.0);
    let title = painter.layout(track.title.clone(), font(Weight::Bold, size), p.text, width);
    let top = cover.bottom() + 20.0 + dy;
    let title_h = title.size().y.min(size * 2.6);
    painter
        .with_clip_rect(Rect::from_min_size(
            pos2(cover.center().x - width / 2.0, top),
            vec2(width, title_h),
        ))
        .galley(
            pos2(cover.center().x - title.size().x / 2.0, top),
            title,
            p.text,
        );
    let artists = runs_text(&track_line(track));
    let line = painter.layout_no_wrap(artists, font(Weight::Regular, size * 0.62), p.secondary);
    let x = cover.center().x - line.size().x.min(width) / 2.0;
    painter
        .with_clip_rect(Rect::from_min_size(
            pos2(cover.center().x - width / 2.0, top + title_h + 4.0),
            vec2(width, line.size().y),
        ))
        .galley(pos2(x, top + title_h + 4.0), line, p.secondary);
}

#[allow(clippy::too_many_arguments)]
fn words_view(
    ui: &mut Ui,
    app: &App,
    track: &Track,
    words: &Words,
    rect: Rect,
    p: &Palette,
    actions: &mut Vec<Action>,
    current: bool,
) {
    match words {
        Words::Timed(lyrics) => timed(ui, app, lyrics, &track.video_id, rect, p, actions, current),
        Words::Plain(lyrics) => plain(ui, app, lyrics, &track.video_id, rect, p, current),
        Words::None => {}
    }
}

/// Timed lyrics in large type: the current line lit in the cover's accent
/// about a third of the way down, upcoming lines readable below it, past
/// lines drifting up and fading. Clicking a line seeks to it.
#[allow(clippy::too_many_arguments)]
fn timed(
    ui: &mut Ui,
    app: &App,
    lyrics: &Lyrics,
    id: &str,
    rect: Rect,
    p: &Palette,
    actions: &mut Vec<Action>,
    current: bool,
) {
    let ctx = ui.ctx().clone();
    let position = app.position_now();
    // The words of a song that just left stay where they were as they fade.
    let shown_id = Id::new(("stage-lyrics-shown", id));
    let kept: Option<(f32, Option<usize>)> = if current {
        None
    } else {
        ctx.data(|d| d.get_temp(shown_id))
    };
    let lit = if current {
        crate::lyrics::current_line(&lyrics.lines, position)
    } else {
        kept.and_then(|k| k.1)
    };
    let size = (rect.height() * 0.05).clamp(22.0, 44.0);
    let gap = size * 0.5;
    let width = rect.width().max(120.0);
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    // Lines measured once per song and size; only the ones shown are laid out.
    let line_font = font(Weight::Bold, size);
    let m = super::lyrics::metrics(
        &painter,
        lyrics,
        id,
        &line_font,
        width,
        gap,
        ("stage", current),
    );
    let anchor = rect.height() * 0.36;
    let target = match lit {
        Some(i) => m.tops[i] + m.heights[i] / 2.0 - anchor,
        None => -anchor + size,
    };
    let (offset, warm) = if current {
        let offset = motion::spring(&ctx, Id::new(("stage-lyrics", id)), target, 60.0);
        // The newly lit line warms into the accent.
        let lit_key = lit.map_or(String::new(), |i| i.to_string());
        let warm = motion::changed(&ctx, Id::new(("stage-lit", id)), &lit_key, 0.35)
            .map_or(1.0, |(_, t)| motion::ease_out(t));
        ctx.data_mut(|d| d.insert_temp(shown_id, (offset, lit)));
        (offset, warm)
    } else {
        (kept.map_or(target, |k| k.0), 1.0)
    };
    let anchor_y = rect.top() + anchor;
    for i in m.first_visible(offset)..lyrics.lines.len() {
        let top = rect.top() + m.tops[i] - offset;
        if top > rect.bottom() {
            break;
        }
        let line_rect = Rect::from_min_size(pos2(rect.left(), top), vec2(width, m.heights[i]));
        if !line_rect.intersects(rect) {
            continue;
        }
        let text = super::lyrics::line_text(&lyrics.lines[i]);
        let colour = match lit {
            Some(c) if c == i => p.text.lerp_to_gamma(p.accent, warm),
            Some(c) if i < c => {
                // Past lines drift up and fade towards the top.
                let rise = ((anchor_y - line_rect.bottom()) / anchor.max(1.0)).clamp(0.0, 1.0);
                p.text.gamma_multiply(0.42 * (1.0 - rise))
            }
            _ => {
                // Upcoming lines, fading out towards the transport.
                let fall = ((line_rect.bottom() - (rect.bottom() - size * 2.5)) / (size * 2.5))
                    .clamp(0.0, 1.0);
                p.text.gamma_multiply(0.72 * (1.0 - fall))
            }
        };
        if current {
            let hit = line_rect.expand2(vec2(10.0, gap / 2.0)).intersect(rect);
            let response = ui.interact(hit, Id::new(("stage-lyric", id, i)), Sense::click());
            if response.hovered() {
                painter.rect_filled(
                    hit,
                    CornerRadius::same(8),
                    p.surface_hover.gamma_multiply(0.5),
                );
            }
            if named(response, text)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                actions.push(Action::Command(Command::Seek(lyrics.lines[i].start)));
            }
        }
        let galley = painter.layout(
            text.to_owned(),
            line_font.clone(),
            Color32::PLACEHOLDER,
            width,
        );
        painter.galley(line_rect.min, galley, colour);
    }
    if let Some(source) = &lyrics.source {
        let top = rect.top() + m.total + gap - offset;
        if top < rect.bottom() {
            painter.text(
                pos2(rect.left(), top),
                Align2::LEFT_TOP,
                source,
                font(Weight::Regular, 13.0),
                p.dim,
            );
        }
    }
    if current && app.playback.playing {
        let next = lit.map_or(0, |c| c + 1);
        if let Some(line) = lyrics.lines.get(next) {
            let wait = (line.start - position).max(0.0);
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
        }
    }
}

/// Plain lyrics scroll gently with the song.
fn plain(ui: &Ui, app: &App, lyrics: &Lyrics, id: &str, rect: Rect, p: &Palette, current: bool) {
    let size = (rect.height() * 0.034).clamp(18.0, 30.0);
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let galley = painter.layout(
        lyrics.text.clone(),
        font(Weight::SemiBold, size),
        Color32::PLACEHOLDER,
        rect.width().max(120.0),
    );
    let travel = (galley.size().y - rect.height() * 0.7).max(0.0);
    let shown_id = Id::new(("stage-plain-shown", id));
    let offset = if current {
        let duration = app.playback.duration;
        let progress = if duration > 0.0 {
            (app.position_now() / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let offset = motion::spring(
            ui.ctx(),
            Id::new(("stage-plain", id)),
            travel * progress,
            40.0,
        );
        ui.ctx().data_mut(|d| d.insert_temp(shown_id, offset));
        offset
    } else {
        // A song that just left keeps its place as it fades.
        ui.ctx()
            .data(|d| d.get_temp::<f32>(shown_id))
            .unwrap_or(0.0)
    };
    painter.galley(
        pos2(rect.left(), rect.top() - offset),
        galley,
        p.text.gamma_multiply(0.8),
    );
    if current && app.playback.playing && travel > 0.0 {
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }
}

/// The close button, transport and seek line with its ridge. They fade
/// away (with the pointer) while the pointer rests, and return when it
/// moves. Returns how visible they are.
fn chrome(
    app: &App,
    ui: &mut Ui,
    rect: Rect,
    margin: f32,
    p: &Palette,
    actions: &mut Vec<Action>,
) -> f32 {
    let ctx = ui.ctx().clone();
    let still = ctx.input(|i| i.pointer.time_since_last_movement());
    let pointer = ctx.pointer_hover_pos();
    let band = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 120.0), rect.max);
    let close_rect = Rect::from_min_size(
        pos2(
            rect.right() - margin * 0.5 - 44.0,
            rect.top() + margin * 0.5,
        ),
        Vec2::splat(44.0),
    );
    let over = pointer.is_some_and(|pos| band.contains(pos) || close_rect.contains(pos));
    let shown = still < IDLE || over;
    if still < IDLE {
        ctx.request_repaint_after(Duration::from_secs_f32(IDLE - still + 0.02));
    }
    let k = motion::spring(
        &ctx,
        Id::new("stage-chrome"),
        if shown { 1.0 } else { 0.0 },
        if shown { 220.0 } else { 40.0 },
    );
    if !shown && k < 0.05 && pointer.is_some() {
        ctx.set_cursor_icon(egui::CursorIcon::None);
    }
    let mut ui = ui.new_child(UiBuilder::new().max_rect(rect));
    ui.multiply_opacity(k);

    // Close.
    let mut corner = ui.new_child(UiBuilder::new().max_rect(close_rect));
    if icon_button(&mut corner, Icon::Close, 24.0, p.text, p, "Close stage").clicked() {
        actions.push(Action::Stage(false));
    }

    // Seek line and its ridge, across the bottom.
    let pb = &app.playback;
    let duration = pb.duration.max(0.0);
    let line_y = rect.bottom() - 44.0;
    let left = rect.left() + margin;
    let right = rect.right() - margin;
    let heat = app.current_heat();
    let hit = Rect::from_min_max(
        pos2(left, line_y - if heat.is_some() { 30.0 } else { 10.0 }),
        pos2(right, line_y + 10.0),
    );
    let seek = named_as(
        ui.interact(hit, Id::new("stage-seek"), Sense::click_and_drag()),
        egui::WidgetType::Slider,
        "Seek",
    );
    let active = seek.hovered() || seek.dragged();
    let open = motion::spring(
        &ctx,
        Id::new("stage-seek-open"),
        if active { 1.0 } else { 0.0 },
        300.0,
    );
    let fraction_at = |pos: egui::Pos2| ((pos.x - left) / (right - left)).clamp(0.0, 1.0);
    let shown_position = match (seek.dragged(), seek.interact_pointer_pos()) {
        (true, Some(pos)) => f64::from(fraction_at(pos)) * duration,
        _ => app.position_now(),
    };
    if (seek.clicked() || seek.drag_stopped())
        && duration > 0.0
        && let Some(pos) = seek.interact_pointer_pos()
    {
        actions.push(Action::Command(Command::Seek(
            f64::from(fraction_at(pos)) * duration,
        )));
    }
    let played = if duration > 0.0 {
        (shown_position / duration) as f32
    } else {
        0.0
    };
    let thickness = 3.0 + 2.0 * open;
    let line = Rect::from_min_max(
        pos2(left, line_y - thickness / 2.0),
        pos2(right, line_y + thickness / 2.0),
    );
    if let Some(heat) = heat
        && duration > 0.0
    {
        let look = ridge::Look {
            fill: p.accent.gamma_multiply(0.2 + 0.15 * open),
            played: p.accent.gamma_multiply(0.38 + 0.2 * open),
            edge: p.accent.gamma_multiply(0.45 + 0.35 * open),
            peak: p.accent,
        };
        let height = 12.0 + 14.0 * open;
        let crest = ridge::paint(
            ui.painter(),
            heat,
            duration,
            line,
            height,
            played,
            open,
            &look,
        );
        if let Some(crest) = crest
            && open > 0.3
        {
            ridge::peak_label(ui.painter(), crest, line, open, p);
        }
    }
    let painter = ui.painter();
    painter.rect_filled(line, CornerRadius::same(2), p.text.gamma_multiply(0.2));
    painter.rect_filled(
        line.with_max_x(left + (right - left) * played.clamp(0.0, 1.0)),
        CornerRadius::same(2),
        p.accent,
    );
    if open > 0.02 {
        painter.circle_filled(
            pos2(left + (right - left) * played.clamp(0.0, 1.0), line_y),
            7.0 * open,
            p.accent,
        );
    }
    if active && let Some(pos) = seek.hover_pos() {
        let at = f64::from(fraction_at(pos)) * duration;
        let text = match heat.and_then(|h| h.peak) {
            Some(peak) if at >= peak.start && at <= peak.end => {
                format!("{} · Most replayed", format_time(at))
            }
            _ => format_time(at),
        };
        seek.clone().on_hover_text_at_pointer(text);
    }
    for (text, align, x) in [
        (format_time(shown_position), Align2::LEFT_TOP, left),
        (format_time(duration), Align2::RIGHT_TOP, right),
    ] {
        painter.text(
            pos2(x, line_y + 10.0),
            align,
            text,
            font(Weight::Regular, 13.0),
            p.secondary,
        );
    }

    // Transport, centred above the line.
    let row = Rect::from_center_size(pos2(rect.center().x, line_y - 44.0), vec2(220.0, 64.0));
    let mut buttons = ui.new_child(
        UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    buttons.spacing_mut().item_spacing.x = 18.0;
    if icon_button(&mut buttons, Icon::SkipBack, 28.0, p.text, p, "Previous").clicked() {
        actions.push(Action::Command(Command::Previous));
    }
    let (play_rect, play) = buttons.allocate_exact_size(Vec2::splat(60.0), Sense::click());
    let lift = motion::lift(&buttons, play.id, play.hovered());
    buttons.painter().circle_filled(
        play_rect.center(),
        28.0 + 2.0 * lift,
        p.text.gamma_multiply(0.92),
    );
    let glyph = if pb.playing { Icon::Pause } else { Icon::Play };
    let nudge = if pb.playing { 0.0 } else { 2.0 };
    glyph.image(p.window, 26.0).paint_at(
        &buttons,
        Rect::from_center_size(play_rect.center() + vec2(nudge, 0.0), Vec2::splat(26.0)),
    );
    let label = if pb.playing { "Pause" } else { "Play" };
    if named(play, label)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        actions.push(Action::Command(Command::TogglePause));
    }
    if icon_button(&mut buttons, Icon::SkipForward, 28.0, p.text, p, "Next").clicked() {
        actions.push(Action::Command(Command::Next));
    }
    k
}
