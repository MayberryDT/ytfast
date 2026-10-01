use crate::app::Action;
use crate::icons::Icon;
use crate::model::{Run, Track};
use crate::theme::Palette;
use egui::{Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke, Ui, Vec2, pos2, vec2};
use fastframe_fonts::Weight;

pub(super) fn font(weight: Weight, size: f32) -> FontId {
    weight.font_id(size)
}

pub(super) fn label(
    ui: &mut Ui,
    text: impl Into<String>,
    size: f32,
    weight: Weight,
    color: Color32,
) -> egui::Response {
    ui.add(
        egui::Label::new(
            RichText::new(text.into())
                .font(font(weight, size))
                .color(color),
        )
        .truncate()
        .selectable(false),
    )
}

/// A clickable line of runs; linked runs open their page.
pub(super) fn runs_line(
    ui: &mut Ui,
    runs: &[Run],
    size: f32,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal(|ui| {
            ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
            for run in runs {
                let text = RichText::new(&run.text)
                    .font(font(Weight::Regular, size))
                    .color(p.secondary);
                match &run.target {
                    Some(target) => {
                        let response = ui.add(
                            egui::Label::new(text)
                                .sense(Sense::click())
                                .selectable(false),
                        );
                        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
                        if response.hovered() {
                            let r = response.rect;
                            ui.painter().line_segment(
                                [pos2(r.left(), r.bottom()), pos2(r.right(), r.bottom())],
                                Stroke::new(1.0, p.secondary),
                            );
                        }
                        if response.clicked() {
                            actions.push(Action::Activate(target.clone()));
                        }
                    }
                    None => {
                        ui.add(egui::Label::new(text).selectable(false));
                    }
                }
            }
        });
    });
}

pub(super) fn runs_text(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// "Artist, Artist • Album", with each name linked to its page.
pub(super) fn track_line(track: &Track) -> Vec<Run> {
    let mut line = track.artists.clone();
    if let Some(album) = &track.album {
        if !line.is_empty() {
            line.push(Run {
                text: " • ".into(),
                target: None,
            });
        }
        line.push(album.clone());
    }
    line
}

/// Names a custom-drawn control for screen readers (AccessKit) and, in E2E
/// runs, for the driver that clicks controls by name.
pub(super) fn named(response: egui::Response, label: &str) -> egui::Response {
    named_as(response, egui::WidgetType::Button, label)
}

pub(super) fn named_as(
    response: egui::Response,
    kind: egui::WidgetType,
    label: &str,
) -> egui::Response {
    let text = label.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(kind, true, &text));
    #[cfg(feature = "e2e")]
    crate::e2e::register(&response.ctx, label, response.interact_rect);
    response
}

/// The pointer has rested on `response` for a moment: a likely click.
pub(super) fn resting(ui: &Ui, response: &egui::Response) -> bool {
    if !response.hovered() {
        return false;
    }
    let still = ui.input(|i| i.pointer.time_since_last_movement());
    if still < 0.4 {
        // egui draws nothing while the pointer is still; come back to check.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f32(0.45 - still));
        return false;
    }
    true
}

/// A round icon button with a hover disc.
pub(super) fn icon_button(
    ui: &mut Ui,
    icon: Icon,
    size: f32,
    color: Color32,
    p: &Palette,
    tip: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size + 16.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .circle_filled(rect.center(), rect.width() / 2.0, p.surface_hover);
    }
    icon.image(color, size)
        .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(size)));
    named(response, tip)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tip)
}

/// A pill button, filled with the accent when `primary`.
pub(super) fn pill(
    ui: &mut Ui,
    text: &str,
    icon: Option<Icon>,
    primary: bool,
    p: &Palette,
) -> egui::Response {
    let (fill, fg) = if primary {
        (p.accent, p.on_accent)
    } else {
        (p.surface, p.text)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font(Weight::Medium, 14.0), fg);
    let icon_w = if icon.is_some() { 22.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 32.0, 36.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.hovered() {
        if primary {
            p.accent_hover
        } else {
            p.surface_hover
        }
    } else {
        fill
    };
    ui.painter().rect_filled(rect, CornerRadius::same(18), fill);
    let mut x = rect.left() + 16.0;
    if let Some(icon) = icon {
        icon.image(fg, 18.0).paint_at(
            ui,
            Rect::from_min_size(pos2(x, rect.center().y - 9.0), Vec2::splat(18.0)),
        );
        x += icon_w;
    }
    ui.painter()
        .galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, fg);
    named(response, text).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Cover art with a placeholder of the same shape while it loads.
pub(super) fn cover(
    ui: &mut Ui,
    rect: Rect,
    url: Option<&str>,
    round: bool,
    radius: u8,
    p: &Palette,
) {
    let corner = if round {
        CornerRadius::same((rect.width() / 2.0).min(255.0) as u8)
    } else {
        CornerRadius::same(radius)
    };
    ui.painter().rect_filled(rect, corner, p.surface);
    match url {
        Some(url) => {
            egui::Image::new(url)
                .corner_radius(corner)
                .show_loading_spinner(false)
                .paint_at(ui, rect);
        }
        None => {
            let s = (rect.width() * 0.35).clamp(14.0, 64.0);
            Icon::Music
                .image(p.dim, s)
                .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(s)));
        }
    }
}

pub(super) fn play_disc(ui: &mut Ui, center: egui::Pos2, radius: f32, p: &Palette, hovered: bool) {
    let fill = if hovered { p.accent_hover } else { p.accent };
    ui.painter().circle_filled(center, radius, fill);
    let s = radius * 0.95;
    Icon::Play.image(p.on_accent, s).paint_at(
        ui,
        Rect::from_center_size(center + vec2(radius * 0.06, 0.0), Vec2::splat(s)),
    );
}

pub(super) fn chip(ui: &mut Ui, text: &str, selected: bool, p: &Palette) -> egui::Response {
    let (fill, fg) = if selected {
        (p.text, p.window)
    } else {
        (p.surface, p.text)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font(Weight::Medium, 14.0), fg);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 24.0, 32.0), Sense::click());
    let fill = if response.hovered() && !selected {
        p.surface_hover
    } else {
        fill
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    named(response, text).on_hover_cursor(egui::CursorIcon::PointingHand)
}
