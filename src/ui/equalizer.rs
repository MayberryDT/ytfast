//! The equalizer, opened from Settings: presets as chips, ten bands as
//! vertical sliders, and a switch that bypasses it in one click.

use super::widgets::{chip, font, icon_button, label, named_as};
use crate::app::{Action, App};
use crate::backend::Command;
use crate::equalizer::{BANDS, Equalizer, Preset, RANGE};
use crate::icons::Icon;
use crate::theme::Palette;
use egui::{
    Align, Align2, CornerRadius, Frame, Id, Layout, Margin, Rect, Sense, Stroke, Ui, pos2, vec2,
};
use fastframe_fonts::Weight;

const COLUMN: f32 = 52.0;
const HEIGHT: f32 = 220.0;

pub(super) fn equalizer(app: &App, ctx: &egui::Context, p: &Palette, actions: &mut Vec<Action>) {
    let eq = &app.playback.equalizer;
    let modal = egui::Modal::new(Id::new("equalizer"))
        .frame(
            Frame::new()
                .fill(p.panel)
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(24)),
        )
        .show(ctx, |ui| {
            ui.set_width(COLUMN * BANDS.len() as f32 + 48.0);
            ui.horizontal(|ui| {
                label(ui, "Equalizer", 22.0, Weight::Bold, p.text);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Close, 18.0, p.secondary, p, "Close equalizer")
                        .clicked()
                    {
                        actions.push(Action::ShowEqualizer(false));
                    }
                    ui.add_space(8.0);
                    switch(ui, eq, p, actions);
                });
            });
            ui.add_space(16.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                for preset in Preset::ALL {
                    if chip(ui, preset.label(), eq.preset == preset, p).clicked() {
                        actions.push(Action::Command(Command::Equalizer(eq.with_preset(preset))));
                    }
                }
                if eq.preset == Preset::Custom {
                    chip(ui, Preset::Custom.label(), true, p);
                }
            });
            ui.add_space(20.0);
            bands(ui, eq, p, actions);
        });
    if modal.should_close() {
        actions.push(Action::ShowEqualizer(false));
    }
}

/// On or off: off bypasses the filter and keeps the bands.
fn switch(ui: &mut Ui, eq: &Equalizer, p: &Palette, actions: &mut Vec<Action>) {
    let (rect, response) = ui.allocate_exact_size(vec2(44.0, 24.0), Sense::click());
    let on = eq.enabled;
    let fill = if on { p.accent } else { p.surface_active };
    ui.painter().rect_filled(rect, CornerRadius::same(12), fill);
    let knob = if on {
        rect.right() - 12.0
    } else {
        rect.left() + 12.0
    };
    ui.painter().circle_filled(
        pos2(knob, rect.center().y),
        8.0,
        if on { p.on_accent } else { p.text },
    );
    let response = named_as(response, egui::WidgetType::Checkbox, "Equalizer")
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if on {
            "Turn the equalizer off"
        } else {
            "Turn the equalizer on"
        });
    if response.clicked() {
        actions.push(Action::Command(Command::Equalizer(Equalizer {
            enabled: !on,
            ..eq.clone()
        })));
    }
    ui.add_space(6.0);
    label(
        ui,
        if on { "On" } else { "Off" },
        14.0,
        Weight::Medium,
        p.secondary,
    );
}

/// Ten vertical sliders, ±12 dB around a 0 dB line.
fn bands(ui: &mut Ui, eq: &Equalizer, p: &Palette, actions: &mut Vec<Action>) {
    let (area, _) = ui.allocate_exact_size(
        vec2(COLUMN * BANDS.len() as f32 + 40.0, HEIGHT + 44.0),
        Sense::hover(),
    );
    let scale = Rect::from_min_size(area.min, vec2(36.0, HEIGHT));
    let top = area.top() + 12.0;
    let bottom = area.top() + HEIGHT - 12.0;
    let zero = (top + bottom) / 2.0;
    let half = (bottom - top) / 2.0;
    let y_of = |gain: f32| zero - gain / RANGE * half;
    let painter = ui.painter().clone();
    for (text, gain) in [("+12", RANGE), ("0", 0.0), ("−12", -RANGE)] {
        painter.text(
            pos2(scale.right() - 6.0, y_of(gain)),
            Align2::RIGHT_CENTER,
            text,
            font(Weight::Regular, 11.5),
            p.dim,
        );
    }
    let columns_left = scale.right() + 4.0;
    painter.hline(
        columns_left..=area.right(),
        zero,
        Stroke::new(1.0, p.outline),
    );
    let tint = if eq.enabled { p.accent } else { p.dim };
    for (i, gain) in eq.gains.iter().copied().enumerate() {
        let column = Rect::from_min_size(
            pos2(columns_left + COLUMN * i as f32, area.top()),
            vec2(COLUMN, HEIGHT),
        );
        let name = Equalizer::band_label(i);
        let response = named_as(
            ui.interact(column, Id::new(("eq-band", i)), Sense::click_and_drag()),
            egui::WidgetType::Slider,
            &name,
        )
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
        let x = column.center().x;
        painter.line_segment(
            [pos2(x, top), pos2(x, bottom)],
            Stroke::new(4.0, p.surface_active),
        );
        let y = y_of(gain);
        painter.line_segment([pos2(x, zero), pos2(x, y)], Stroke::new(4.0, tint));
        let active = response.hovered() || response.dragged();
        painter.circle(
            pos2(x, y),
            if active { 9.0 } else { 7.5 },
            tint,
            Stroke::new(2.0, p.panel),
        );
        if active {
            painter.text(
                pos2(x, top - 6.0),
                Align2::CENTER_BOTTOM,
                format!("{gain:+.1}"),
                font(Weight::Medium, 12.0),
                p.text,
            );
        }
        painter.text(
            pos2(x, area.top() + HEIGHT + 10.0),
            Align2::CENTER_TOP,
            short_label(i),
            font(Weight::Medium, 12.0),
            p.secondary,
        );
        let wanted = if response.double_clicked() {
            Some(0.0)
        } else if response.dragged() || response.clicked() {
            response
                .interact_pointer_pos()
                .map(|pos| ((zero - pos.y) / half * RANGE).clamp(-RANGE, RANGE))
        } else {
            None
        };
        if let Some(wanted) = wanted {
            let next = eq.with_band(i, wanted);
            if next != *eq {
                actions.push(Action::Command(Command::Equalizer(next)));
            }
        }
    }
}

/// "31", "1k": the column labels; the unit is implied.
fn short_label(band: usize) -> String {
    match BANDS.get(band) {
        Some(&f) if f >= 1000 => format!("{}k", f / 1000),
        Some(f) => f.to_string(),
        None => String::new(),
    }
}
