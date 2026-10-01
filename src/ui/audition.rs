//! Audition under the pointer: holding Alt (alone) over a song row or card,
//! or holding the middle button on it, previews the song over the ducked
//! current one (see `backend::audition`). Rows and cards call [`hook`]; the
//! app turns what is held each frame into commands, so letting go, moving
//! off or losing focus ends it on the next frame.

use std::f64::consts::TAU;

use egui::{CornerRadius, Id, Rect, Response, Stroke, StrokeKind, Ui};

use crate::app::Action;
use crate::model::{Audition, Track};
use crate::theme::Palette;

fn shown_id() -> Id {
    Id::new("audition-shown")
}

/// Leaves the audition the backend reports where the widgets can see it.
pub fn publish(ctx: &egui::Context, audition: Option<&Audition>) {
    ctx.data_mut(|d| d.insert_temp(shown_id(), audition.cloned()));
}

/// Where a held song starts when nothing better is known: a third of the
/// way in, or 30 s into long songs (mixes, extended versions), whose thirds
/// are far from their hooks. `None` (unknown length) lets the player take a
/// third once it knows.
pub fn best_part(track: &Track) -> Option<f64> {
    let duration = f64::from(track.duration?);
    Some(if duration > 360.0 {
        30.0
    } else {
        duration / 3.0
    })
}

/// Whether the pointer holds `response` for an audition.
fn holding(ui: &Ui, response: &Response) -> bool {
    response.contains_pointer()
        && ui.input(|i| {
            let m = i.modifiers;
            (m.alt && !m.ctrl && !m.command && !m.shift) || i.pointer.middle_down()
        })
}

/// A song's row or card: asks for an audition while it is held, and marks
/// the cover (`cover`, round for artists) while it is auditioned.
pub(super) fn hook(
    ui: &Ui,
    response: &Response,
    track: &Track,
    cover: Rect,
    round: bool,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    if holding(ui, response) {
        actions.push(Action::Audition(track.clone()));
    }
    let shown: Option<Audition> = ui.ctx().data(|d| d.get_temp(shown_id())).flatten();
    let Some(shown) = shown.filter(|a| a.video_id == track.video_id) else {
        return;
    };
    let painter = ui.painter();
    if shown.playing {
        // A ring in the accent breathes around the cover while it plays.
        let time = ui.input(|i| i.time);
        let pulse = (0.5 + 0.5 * (time * TAU / 1.1).sin()) as f32;
        let ring = cover.expand(2.0 + 2.0 * pulse);
        let stroke = Stroke::new(2.0, p.accent.gamma_multiply(0.5 + 0.5 * pulse));
        if round {
            painter.circle_stroke(ring.center(), ring.width() / 2.0, stroke);
        } else {
            painter.rect_stroke(ring, CornerRadius::same(7), stroke, StrokeKind::Outside);
        }
        ui.ctx().request_repaint();
    } else {
        // Being prepared: a small spinner on the cover until it can start.
        let radius = if round {
            CornerRadius::same((cover.width() / 2.0) as u8)
        } else {
            CornerRadius::same(4)
        };
        painter.rect_filled(cover, radius, p.overlay);
        let size = (cover.width() * 0.45).clamp(14.0, 28.0);
        egui::Spinner::new().size(size).color(p.text).paint_at(
            ui,
            Rect::from_center_size(cover.center(), egui::Vec2::splat(size)),
        );
    }
}
