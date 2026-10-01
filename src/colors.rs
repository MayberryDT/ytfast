//! Colour from the current cover, for Now Playing (and later Stage): the
//! one place where colour comes from the music instead of the Omarchy theme.
//!
//! A cover is decoded and reduced off the interface thread to two colours: a
//! dominant deep tone and a vivid accent. [`Wash`] turns them into Now
//! Playing's background (a vertical blend in the cover's hue, light or dark
//! as the theme is) and into text and accent colours chosen for contrast
//! against it. Colour maths is done in OKLab, so lightness and chroma can be
//! set without shifting the hue.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use egui::Color32;
use egui::load::{Bytes, BytesPoll};

use crate::theme::Palette;

/// The colours taken from a cover.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoverColors {
    /// The cover's dominant tone (the mean colour of its strongest hue).
    pub deep: Color32,
    /// Its most vivid hue.
    pub accent: Color32,
    /// The cover is (nearly) greyscale: no hue worth taking.
    pub neutral: bool,
}

/// How long Now Playing takes to move to a new cover's colours.
pub const FADE: Duration = Duration::from_millis(400);

// ---- OKLab ----

#[derive(Clone, Copy, Debug)]
struct Lab {
    l: f32,
    a: f32,
    b: f32,
}

impl Lab {
    fn chroma(self) -> f32 {
        self.a.hypot(self.b)
    }

    fn hue(self) -> f32 {
        self.b.atan2(self.a)
    }

    fn lch(l: f32, c: f32, h: f32) -> Self {
        Self {
            l,
            a: c * h.cos(),
            b: c * h.sin(),
        }
    }

    fn mix(self, other: Lab, t: f32) -> Lab {
        Lab {
            l: self.l + (other.l - self.l) * t,
            a: self.a + (other.a - self.a) * t,
            b: self.b + (other.b - self.b) * t,
        }
    }
}

fn to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

fn linear_to_lab([r, g, b]: [f32; 3]) -> Lab {
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    Lab {
        l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    }
}

fn lab_to_linear(lab: Lab) -> [f32; 3] {
    let l = (lab.l + 0.396_337_78 * lab.a + 0.215_803_76 * lab.b).powi(3);
    let m = (lab.l - 0.105_561_35 * lab.a - 0.063_854_17 * lab.b).powi(3);
    let s = (lab.l - 0.089_484_18 * lab.a - 1.291_485_5 * lab.b).powi(3);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

fn lab(color: Color32) -> Lab {
    linear_to_lab([
        to_linear(color.r()),
        to_linear(color.g()),
        to_linear(color.b()),
    ])
}

/// The colour, with chroma reduced until it fits in sRGB.
fn color(lab: Lab) -> Color32 {
    let in_gamut = |rgb: [f32; 3]| rgb.iter().all(|c| (-0.001..=1.001).contains(c));
    let mut rgb = lab_to_linear(lab);
    if !in_gamut(rgb) {
        let (c, h) = (lab.chroma(), lab.hue());
        let (mut lo, mut hi) = (0.0_f32, c);
        for _ in 0..16 {
            let mid = (lo + hi) / 2.0;
            if in_gamut(lab_to_linear(Lab::lch(lab.l, mid, h))) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        rgb = lab_to_linear(Lab::lch(lab.l, lo, h));
    }
    Color32::from_rgb(
        from_linear(rgb[0]),
        from_linear(rgb[1]),
        from_linear(rgb[2]),
    )
}

/// WCAG relative luminance.
fn luminance(color: Color32) -> f32 {
    0.2126 * to_linear(color.r()) + 0.7152 * to_linear(color.g()) + 0.0722 * to_linear(color.b())
}

/// WCAG contrast ratio between two colours (1 to 21).
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `start` with its lightness moved (towards white if `lighter`, else black)
/// just far enough to reach `min` contrast against every colour in `against`.
fn legible(start: Color32, against: &[Color32], min: f32, lighter: bool) -> Color32 {
    let worst = |c: Color32| {
        against
            .iter()
            .map(|&b| contrast(c, b))
            .fold(f32::INFINITY, f32::min)
    };
    if worst(start) >= min {
        return start;
    }
    let base = lab(start);
    let (c, h) = (base.chroma(), base.hue());
    let end = if lighter { 1.0 } else { 0.0 };
    let at = |t: f32| color(Lab::lch(base.l + (end - base.l) * t, c * (1.0 - t), h));
    if worst(at(1.0)) < min {
        return at(1.0);
    }
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..20 {
        let mid = (lo + hi) / 2.0;
        if worst(at(mid)) >= min {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    at(hi)
}

// ---- extraction ----

const BINS: usize = 24;
/// Below this OKLab chroma a pixel counts as grey.
const GREY: f32 = 0.035;

/// Reduces a cover (JPEG, PNG or WebP bytes) to its colours. Slow enough
/// (decoding) that it never runs on the interface thread.
pub fn extract(bytes: &[u8]) -> Option<CoverColors> {
    let image = image::load_from_memory(bytes).ok()?;
    let small = image.thumbnail(48, 48).to_rgb8();
    let pixels: Vec<Lab> = small
        .pixels()
        .map(|p| linear_to_lab([to_linear(p[0]), to_linear(p[1]), to_linear(p[2])]))
        .collect();
    if pixels.is_empty() {
        return None;
    }
    let bin_of = |p: &Lab| {
        let turn = (p.hue() + std::f32::consts::PI) / std::f32::consts::TAU;
        ((turn * BINS as f32) as usize).min(BINS - 1)
    };
    // How much of the cover each hue covers (favouring mid lightness, where
    // a hue reads as itself), and how vivid each hue is.
    let mut area = [0.0_f32; BINS];
    let mut vivid = [0.0_f32; BINS];
    let mut coloured = 0usize;
    for p in &pixels {
        let c = p.chroma();
        if c < GREY {
            continue;
        }
        coloured += 1;
        let bin = bin_of(p);
        area[bin] += c * (1.0 - (p.l - 0.55).abs());
        vivid[bin] += c * c * c;
    }
    let mean = |pick: &dyn Fn(&Lab) -> f32| {
        let (mut sum, mut weight) = (
            Lab {
                l: 0.0,
                a: 0.0,
                b: 0.0,
            },
            0.0_f32,
        );
        for p in &pixels {
            let w = pick(p);
            if w > 0.0 {
                sum.l += p.l * w;
                sum.a += p.a * w;
                sum.b += p.b * w;
                weight += w;
            }
        }
        (weight > 0.0).then(|| Lab {
            l: sum.l / weight,
            a: sum.a / weight,
            b: sum.b / weight,
        })
    };
    // Nearly greyscale (black-and-white photos, plain type covers).
    if (coloured as f32) < pixels.len() as f32 * 0.06 {
        let average = mean(&|_| 1.0)?;
        let grey = color(average);
        return Some(CoverColors {
            deep: grey,
            accent: grey,
            neutral: true,
        });
    }
    let strongest = |weights: &[f32; BINS]| {
        (0..BINS)
            .max_by(|&x, &y| {
                let around = |i: usize| {
                    weights[(i + BINS - 1) % BINS] * 0.5
                        + weights[i]
                        + weights[(i + 1) % BINS] * 0.5
                };
                around(x).total_cmp(&around(y))
            })
            .unwrap_or(0)
    };
    let near = |bin: usize, p: &Lab| {
        let d = (bin_of(p) + BINS - bin) % BINS;
        p.chroma() >= GREY && (d <= 1 || d == BINS - 1)
    };
    let dominant = strongest(&area);
    let deep = mean(&|p| if near(dominant, p) { p.chroma() } else { 0.0 })?;
    let striking = strongest(&vivid);
    let accent = mean(&|p| {
        if near(striking, p) {
            p.chroma().powi(3)
        } else {
            0.0
        }
    })?;
    Some(CoverColors {
        deep: color(deep),
        accent: color(accent),
        neutral: false,
    })
}

// ---- the wash ----

/// Now Playing's colours for one cover over the current theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wash {
    /// The background, from the top of the window to the bottom.
    pub top: Color32,
    pub bottom: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
}

impl Wash {
    /// The wash for `colors` (the plain theme without a cover) under palette `p`.
    pub fn new(colors: Option<&CoverColors>, p: &Palette) -> Self {
        let Some(colors) = colors else {
            return Self {
                top: p.window,
                bottom: p.window,
                text: p.text,
                secondary: p.secondary,
                dim: p.dim,
                accent: p.accent,
                on_accent: p.on_accent,
            };
        };
        let deep = lab(colors.deep);
        let (h, c) = (deep.hue(), if colors.neutral { 0.0 } else { deep.chroma() });
        let window = lab(p.window);
        // A calm field: the cover's hue at a lightness the theme sets, a
        // little of the theme's own background mixed in.
        let (top, bottom) = if p.dark {
            (
                Lab::lch(0.31, c.min(0.085), h),
                Lab::lch(0.17, c.min(0.085) * 0.55, h),
            )
        } else {
            (
                Lab::lch(0.89, c.min(0.06), h),
                Lab::lch(0.965, c.min(0.03), h),
            )
        };
        let top = color(top.mix(window, 0.18));
        let bottom = color(bottom.mix(window, 0.18));
        let field = [top, bottom];
        // Text gets lighter on a dark wash and darker on a light one.
        let lighter = p.dark;
        let text = legible(p.text, &field, 7.0, lighter);
        let secondary = legible(p.secondary, &field, 4.5, lighter);
        let dim = legible(p.dim, &field, 3.0, lighter);
        let accent_start = if colors.neutral {
            p.accent
        } else {
            let a = lab(colors.accent);
            color(Lab::lch(a.l, a.chroma().clamp(0.1, 0.19), a.hue()))
        };
        let accent = legible(accent_start, &field, 4.5, lighter);
        let on_accent = legible(bottom, &[accent], 4.5, luminance(accent) < 0.18);
        Self {
            top,
            bottom,
            text,
            secondary,
            dim,
            accent,
            on_accent,
        }
    }

    fn lerp(self, to: Wash, t: f32) -> Wash {
        let m = |a: Color32, b: Color32| a.lerp_to_gamma(b, t);
        Wash {
            top: m(self.top, to.top),
            bottom: m(self.bottom, to.bottom),
            text: m(self.text, to.text),
            secondary: m(self.secondary, to.secondary),
            dim: m(self.dim, to.dim),
            accent: m(self.accent, to.accent),
            on_accent: m(self.on_accent, to.on_accent),
        }
    }

    /// The theme palette with the wash's colours in it, for everything drawn on the wash.
    pub fn palette(&self, base: &Palette) -> Palette {
        let field = self.top.lerp_to_gamma(self.bottom, 0.5);
        let over = |t: f32| field.lerp_to_gamma(self.text, t);
        Palette {
            window: field,
            panel: field,
            surface: over(0.08),
            surface_hover: over(0.13),
            surface_active: over(0.19),
            outline: over(0.24),
            text: self.text,
            secondary: self.secondary,
            dim: self.dim,
            accent: self.accent,
            accent_hover: self.accent.lerp_to_gamma(self.text, 0.2),
            on_accent: self.on_accent,
            overlay: self.bottom.gamma_multiply(0.8),
            ..base.clone()
        }
    }

    /// Fills `rect` with the wash's vertical blend.
    pub fn paint(&self, painter: &egui::Painter, rect: egui::Rect) {
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(rect.left_top(), self.top);
        mesh.colored_vertex(rect.right_top(), self.top);
        mesh.colored_vertex(rect.right_bottom(), self.bottom);
        mesh.colored_vertex(rect.left_bottom(), self.bottom);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
    }
}

/// The move from one cover's colours to the next.
#[derive(Default)]
pub struct Fade {
    from: Option<(Wash, Instant)>,
}

impl Fade {
    /// The wash to draw for the cover colours `to`, and whether it is still moving.
    pub fn wash(&self, to: Option<&CoverColors>, p: &Palette) -> (Wash, bool) {
        let target = Wash::new(to, p);
        match self.from {
            Some((from, since)) if since.elapsed() < FADE => {
                let t = since.elapsed().as_secs_f32() / FADE.as_secs_f32();
                let eased = 1.0 - (1.0 - t).powi(3);
                (from.lerp(target, eased), true)
            }
            _ => (target, false),
        }
    }

    /// Starts moving from what is shown now (for colours `shown`) to new colours.
    pub fn start(&mut self, shown: Option<&CoverColors>, p: &Palette) {
        let (now, _) = self.wash(shown, p);
        self.from = Some((now, Instant::now()));
    }
}

/// Extracts covers' colours on the backend runtime and remembers them.
pub struct Extractor {
    known: HashMap<String, Option<CoverColors>>,
    working: HashSet<String>,
    tx: mpsc::Sender<(String, Option<CoverColors>)>,
    rx: mpsc::Receiver<(String, Option<CoverColors>)>,
}

impl Default for Extractor {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            known: HashMap::new(),
            working: HashSet::new(),
            tx,
            rx,
        }
    }
}

impl Extractor {
    /// The colours of the cover at `url`: `None` while they are being worked
    /// out, `Some(None)` if the cover has none to give. Uses the bytes the
    /// cover loader already holds (the same ones the cover is drawn from).
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        runtime: &tokio::runtime::Handle,
        url: &str,
    ) -> Option<Option<CoverColors>> {
        while let Ok((done, colors)) = self.rx.try_recv() {
            self.working.remove(&done);
            if self.known.len() > 64 {
                self.known.clear();
            }
            self.known.insert(done, colors);
        }
        if let Some(colors) = self.known.get(url) {
            return Some(*colors);
        }
        if self.working.contains(url) {
            return None;
        }
        match ctx.try_load_bytes(url) {
            Ok(BytesPoll::Ready { bytes, .. }) => {
                let bytes: Arc<[u8]> = match bytes {
                    Bytes::Shared(shared) => shared,
                    Bytes::Static(slice) => Arc::from(slice),
                };
                self.working.insert(url.to_owned());
                let tx = self.tx.clone();
                let ctx = ctx.clone();
                let url = url.to_owned();
                runtime.spawn_blocking(move || {
                    let colors = extract(&bytes);
                    let _ = tx.send((url, colors));
                    ctx.request_repaint();
                });
                None
            }
            // The loader repaints when the bytes arrive.
            Ok(BytesPoll::Pending { .. }) => None,
            // No cover to take colour from (the loader retries later).
            Err(_) => Some(None),
        }
    }
}
