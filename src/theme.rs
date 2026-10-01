//! Colours: the Omarchy theme's palette, applied to egui.
//!
//! ytfast follows the desktop's theme (fastframe-theme renders Omarchy's
//! current colours into the sixteen base roles). The built-in palettes are
//! only the fallback when no Omarchy theme can be read.

use std::collections::BTreeSet;

use egui::{Color32, CornerRadius, Stroke, Visuals};

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub window: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub surface_active: Color32,
    pub outline: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub overlay: Color32,
    pub shadow: Color32,
}

impl Palette {
    fn neutral(dark: bool) -> Self {
        let g = |v: u8| Color32::from_gray(v);
        if dark {
            Self {
                dark,
                window: g(18),
                panel: g(24),
                surface: g(34),
                surface_hover: g(44),
                surface_active: g(56),
                outline: g(60),
                text: g(236),
                secondary: g(170),
                dim: g(120),
                accent: g(236),
                accent_hover: g(255),
                on_accent: g(18),
                danger: Color32::from_rgb(230, 90, 90),
                warning: Color32::from_rgb(230, 190, 90),
                overlay: Color32::from_black_alpha(160),
                shadow: Color32::from_black_alpha(90),
            }
        } else {
            Self {
                dark,
                window: g(250),
                panel: g(242),
                surface: g(232),
                surface_hover: g(222),
                surface_active: g(208),
                outline: g(200),
                text: g(20),
                secondary: g(80),
                dim: g(120),
                accent: g(20),
                accent_hover: g(0),
                on_accent: g(250),
                danger: Color32::from_rgb(190, 40, 40),
                warning: Color32::from_rgb(170, 120, 0),
                overlay: Color32::from_black_alpha(110),
                shadow: Color32::from_black_alpha(40),
            }
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::neutral(true)
    }
}

impl fastframe_theme::Palette for Palette {
    fn base(base: fastframe_theme::Base) -> Self {
        Self::neutral(base == fastframe_theme::Base::Dark)
    }

    fn set(&mut self, name: &str, color: Color32) -> bool {
        let slot = match name {
            "window" => &mut self.window,
            "panel" => &mut self.panel,
            "surface" => &mut self.surface,
            "surface_hover" => &mut self.surface_hover,
            "surface_active" => &mut self.surface_active,
            "outline" => &mut self.outline,
            "text" => &mut self.text,
            "secondary" => &mut self.secondary,
            "dim" => &mut self.dim,
            "accent" => &mut self.accent,
            "accent_hover" => &mut self.accent_hover,
            "on_accent" => &mut self.on_accent,
            "danger" => &mut self.danger,
            "warning" => &mut self.warning,
            "overlay" => &mut self.overlay,
            "shadow" => &mut self.shadow,
            _ => return false,
        };
        *slot = color;
        true
    }

    fn derive(&mut self, given: &BTreeSet<&str>) {
        if given.contains("window") && !given.contains("overlay") {
            self.overlay = self.window.gamma_multiply(0.85);
        }
    }
}

/// Maps the palette onto egui's visuals.
pub fn apply(ctx: &egui::Context, p: &Palette) {
    let mut v = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.window;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.outline);
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.panel;
    v.code_bg_color = p.surface;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow.color = p.shadow;
    v.popup_shadow.color = p.shadow;
    for (w, fill) in [
        (&mut v.widgets.noninteractive, p.window),
        (&mut v.widgets.inactive, p.surface),
        (&mut v.widgets.hovered, p.surface_hover),
        (&mut v.widgets.active, p.surface_active),
        (&mut v.widgets.open, p.surface_hover),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.fg_stroke = Stroke::new(1.0, p.text);
        w.bg_stroke = Stroke::NONE;
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.outline);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.secondary);
    v.slider_trailing_fill = true;
    ctx.set_visuals(v);
}
