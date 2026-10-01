//! The most-replayed ridge: YouTube's replay heat drawn as a low, smooth
//! hill along a seek line, its peak marked. Low and quiet at rest, fuller
//! under the pointer; a click on it seeks there.

use super::motion;
use super::widgets::font;
use crate::app::{Action, App};
use crate::backend::Command;
use crate::heat::Heat;
use crate::model::format_time;
use crate::theme::Palette;
use egui::{Color32, CornerRadius, Id, Painter, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use fastframe_fonts::Weight;

/// Height at rest and fully open, in points.
const REST: f32 = 7.0;
const OPEN: f32 = 28.0;

/// The colours a ridge is drawn in.
pub(super) struct Look {
    /// Ahead of the playhead, and behind it.
    pub fill: Color32,
    pub played: Color32,
    pub edge: Color32,
    pub peak: Color32,
}

/// The heat as (x, 0..=1) points along `line`'s width: lightly smoothed,
/// then a Catmull-Rom curve through the markers' middles.
fn curve(heat: &Heat, duration: f64, line: Rect) -> Vec<(f32, f32)> {
    let m = &heat.markers;
    let x = |t: f64| line.left() + (t / duration).clamp(0.0, 1.0) as f32 * line.width();
    let smooth = |i: usize| {
        let at = |j: isize| m[j.clamp(0, m.len() as isize - 1) as usize].intensity;
        let i = i as isize;
        (at(i - 1) + 2.0 * at(i) + at(i + 1)) / 4.0
    };
    let mut points = Vec::with_capacity(m.len() + 2);
    points.push((line.left(), smooth(0)));
    for (i, marker) in m.iter().enumerate() {
        points.push((x(marker.start + marker.duration / 2.0), smooth(i)));
    }
    points.push((x(heat.length()), smooth(m.len() - 1)));
    let n = points.len();
    let get = |i: isize| points[i.clamp(0, n as isize - 1) as usize];
    let mut out = Vec::with_capacity(n * 3);
    for i in 0..n as isize - 1 {
        let (p0, p1, p2, p3) = (get(i - 1), get(i), get(i + 1), get(i + 2));
        for k in 0..3 {
            let t = k as f32 / 3.0;
            let cr = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * (2.0 * b
                    + (c - a) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t
                    + (3.0 * b - a - 3.0 * c + d) * t * t * t)
            };
            out.push((
                cr(p0.0, p1.0, p2.0, p3.0),
                cr(p0.1, p1.1, p2.1, p3.1).clamp(0.0, 1.0),
            ));
        }
    }
    out.push(points[n - 1]);
    out
}

fn value_at(points: &[(f32, f32)], x: f32) -> f32 {
    let i = points.partition_point(|p| p.0 < x);
    match (i.checked_sub(1).and_then(|j| points.get(j)), points.get(i)) {
        (Some(a), Some(b)) if b.0 > a.0 => a.1 + (b.1 - a.1) * (x - a.0) / (b.0 - a.0),
        (_, Some(b)) => b.1,
        (Some(a), None) => a.1,
        (None, None) => 0.0,
    }
}

/// Draws the ridge rising from the top of `line`, `height` tall where the
/// heat is greatest, `played` (0..=1) of it behind the playhead. `open`
/// (0..=1) brings out the peak's mark and part.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint(
    painter: &Painter,
    heat: &Heat,
    duration: f64,
    line: Rect,
    height: f32,
    played: f32,
    open: f32,
    look: &Look,
) {
    if duration <= 0.0 || heat.markers.is_empty() {
        return;
    }
    let points = curve(heat, duration, line);
    let base = line.top();
    let top = |(x, v): (f32, f32)| pos2(x, base - height * v);
    let playhead = line.left() + line.width() * played.clamp(0.0, 1.0);
    let span = Rect::from_min_max(
        pos2(line.left() - 2.0, base - height - 4.0),
        pos2(line.right() + 2.0, base),
    );
    for (clip, colour) in [
        (span.with_max_x(playhead), look.played),
        (span.with_min_x(playhead), look.fill),
    ] {
        let clipped = painter.with_clip_rect(clip.intersect(painter.clip_rect()));
        let mut mesh = egui::Mesh::default();
        let foot = colour.gamma_multiply(0.45);
        for (i, &p) in points.iter().enumerate() {
            mesh.colored_vertex(top(p), colour);
            mesh.colored_vertex(pos2(p.0, base), foot);
            if i > 0 {
                let v = (i * 2) as u32;
                mesh.add_triangle(v - 2, v - 1, v);
                mesh.add_triangle(v - 1, v, v + 1);
            }
        }
        clipped.add(egui::Shape::mesh(mesh));
    }
    painter.add(egui::Shape::line(
        points.iter().map(|&p| top(p)).collect(),
        Stroke::new(1.2, look.edge),
    ));
    if let Some(peak) = heat.peak {
        let x = |t: f64| line.left() + (t / duration).clamp(0.0, 1.0) as f32 * line.width();
        let at = x(peak.at);
        if open > 0.02 {
            // The most replayed part, underlined along the line.
            painter.rect_filled(
                Rect::from_min_max(pos2(x(peak.start), base - 2.0), pos2(x(peak.end), base)),
                CornerRadius::same(1),
                look.peak.gamma_multiply(0.7 * open),
            );
        }
        let crest = top((at, value_at(&points, at)));
        painter.circle_filled(crest, 2.2 + 1.6 * open, look.peak);
    }
}

fn last_hover_id() -> Id {
    Id::new("seek-ridge-hovered")
}

/// The player bar's ridge, above its seek line (`bar`, the seek control's
/// area along the bar's top edge) and over the page. Returns whether the
/// pointer is on it, so the seek line can stay open while it is.
pub(super) fn player_ridge(
    ui: &Ui,
    app: &App,
    bar: Rect,
    seek_active: bool,
    p: &Palette,
    actions: &mut Vec<Action>,
) -> bool {
    let ctx = ui.ctx();
    let duration = app.playback.duration;
    let Some(heat) = app.current_heat().filter(|_| duration > 0.0) else {
        ctx.data_mut(|d| d.remove::<bool>(last_hover_id()));
        return false;
    };
    let was_hovered = ctx.data(|d| d.get_temp::<bool>(last_hover_id())) == Some(true);
    let open = motion::spring(
        ctx,
        Id::new("seek-ridge-open"),
        if seek_active || was_hovered { 1.0 } else { 0.0 },
        260.0,
    );
    let height = REST + (OPEN - REST) * open;
    let area = Rect::from_min_max(pos2(bar.left(), bar.top() - height - 6.0), bar.right_top());
    let played = if duration > 0.0 {
        (app.position_now() / duration) as f32
    } else {
        0.0
    };
    let look = Look {
        fill: p.accent.gamma_multiply(0.14 + 0.16 * open),
        played: p.accent.gamma_multiply(0.26 + 0.24 * open),
        edge: p.accent.gamma_multiply(0.3 + 0.4 * open),
        peak: p.accent,
    };
    let hovered = egui::Area::new(Id::new("seek-ridge"))
        .order(egui::Order::Middle)
        .fixed_pos(area.min)
        .constrain(false)
        .fade_in(false)
        // At rest clicks go through to the page below; open, it seeks.
        .interactable(open > 0.05)
        .show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(area.size(), Sense::click_and_drag());
            let line = Rect::from_min_max(pos2(rect.left(), rect.bottom()), rect.right_bottom());
            paint(
                ui.painter(),
                heat,
                duration,
                line,
                height,
                played,
                open,
                &look,
            );
            if let Some(peak) = heat.peak
                && open > 0.3
            {
                peak_label(ui.painter(), heat, peak.at, duration, line, height, open, p);
            }
            let fraction = response
                .interact_pointer_pos()
                .or(response.hover_pos())
                .map(|pos| ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
            if (response.clicked() || response.drag_stopped())
                && let Some(f) = fraction
            {
                actions.push(Action::Command(Command::Seek(f64::from(f) * duration)));
            }
            let hovered = response.hovered() || response.dragged();
            if hovered && let Some(f) = fraction {
                let at = f64::from(f) * duration;
                let text = match heat.peak {
                    Some(peak) if at >= peak.start && at <= peak.end => {
                        format!("{} · Most replayed", format_time(at))
                    }
                    _ => format_time(at),
                };
                response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text_at_pointer(text);
            }
            hovered
        })
        .inner;
    ctx.data_mut(|d| d.insert_temp(last_hover_id(), hovered));
    hovered
}

/// "Most replayed" over the peak, on a small plate so it reads over the page.
#[allow(clippy::too_many_arguments)]
pub(super) fn peak_label(
    painter: &Painter,
    heat: &Heat,
    at: f64,
    duration: f64,
    line: Rect,
    height: f32,
    open: f32,
    p: &Palette,
) {
    let x = line.left() + (at / duration).clamp(0.0, 1.0) as f32 * line.width();
    let points = curve(heat, duration, line);
    let crest = line.top() - height * value_at(&points, x);
    let alpha = ((open - 0.3) / 0.7).clamp(0.0, 1.0);
    let galley = painter.layout_no_wrap(
        "Most replayed".to_owned(),
        font(Weight::Medium, 11.5),
        p.text.gamma_multiply(alpha),
    );
    let size = galley.size() + vec2(12.0, 4.0);
    let left = (x - size.x / 2.0).clamp(line.left(), line.right() - size.x);
    let plate = Rect::from_min_size(Pos2::new(left, crest - size.y - 6.0), size);
    painter.rect_filled(plate, CornerRadius::same(6), p.panel.gamma_multiply(alpha));
    painter.galley(
        plate.center() - galley.size() / 2.0,
        galley,
        p.text.gamma_multiply(alpha),
    );
}
