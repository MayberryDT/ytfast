//! Covers painted in the theme's colours (Settings → Paint covers in theme
//! colours).
//!
//! An egui image loader for `ytfast-paint://<colours>/<cover url>`. It takes
//! the cover's bytes from the cover loader (the same ones the plain cover is
//! drawn from), paints them off the interface thread, and answers `Pending`
//! until done. Each result is small and made once per cover and palette;
//! drawing it costs one textured rectangle.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use egui::load::{BytesPoll, ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint};
use egui::{Color32, ColorImage, Context, Id, Vec2};

use crate::theme::Palette;

const PAINT: &str = "ytfast-paint://";
/// A painted cover's largest side (the page header's cover at 1.5×).
const PAINT_SIZE: u32 = 360;

fn paint_uri(key: &str, url: &str) -> String {
    format!("{PAINT}{key}/{url}")
}

// ---- making the images ----

/// The theme colours a cover is painted in: its shadows in the darker of
/// the theme's background and text, its highlights in the lighter, the
/// accent in between at its own lightness.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Paint {
    shadow: Color32,
    mid: Color32,
    light: Color32,
}

fn gamma_luma(c: Color32) -> f32 {
    (0.2126 * f32::from(c.r()) + 0.7152 * f32::from(c.g()) + 0.0722 * f32::from(c.b())) / 255.0
}

impl Paint {
    fn from_palette(p: &Palette) -> Self {
        let (dark, light) = if gamma_luma(p.window) <= gamma_luma(p.text) {
            (p.window, p.text)
        } else {
            (p.text, p.window)
        };
        // A little apart from the ends, so a painted cover never melts into
        // the background it sits on.
        Self {
            shadow: dark.lerp_to_gamma(light, 0.07),
            mid: p.accent,
            light: light.lerp_to_gamma(dark, 0.08),
        }
    }

    fn key(&self) -> String {
        let hex = |c: Color32| format!("{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
        format!("{}-{}-{}", hex(self.shadow), hex(self.mid), hex(self.light))
    }

    fn parse(key: &str) -> Option<Self> {
        let mut parts = key.split('-').map(|h| {
            if h.len() != 6 {
                return None;
            }
            let v = u32::from_str_radix(h, 16).ok()?;
            Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
        });
        let paint = Self {
            shadow: parts.next()??,
            mid: parts.next()??,
            light: parts.next()??,
        };
        parts.next().is_none().then_some(paint)
    }

    /// Shadow → accent → highlight over the cover's lightness 0..=255. The
    /// accent sits where its own lightness falls, so light and shadow keep
    /// their order and the cover stays recognisable.
    fn ramp(&self) -> Vec<Color32> {
        let (s, l) = (gamma_luma(self.shadow), gamma_luma(self.light));
        let at = if (l - s).abs() > 0.05 {
            ((gamma_luma(self.mid) - s) / (l - s)).clamp(0.3, 0.7)
        } else {
            0.5
        };
        (0..256)
            .map(|i| {
                let t = i as f32 / 255.0;
                if t < at {
                    self.shadow.lerp_to_gamma(self.mid, t / at)
                } else {
                    self.mid.lerp_to_gamma(self.light, (t - at) / (1.0 - at))
                }
            })
            .collect()
    }
}

/// The cover as a duotone/tritone of `paint`, its lightness stretched to
/// the full ramp (ignoring the darkest and lightest 2 %).
fn paint(bytes: &[u8], paint: Paint) -> Option<ColorImage> {
    let image = image::load_from_memory(bytes).ok()?;
    // A smooth downscale: covers come at 544 px, close enough to the size
    // here that a block-average one would alias.
    let small = if image.width().max(image.height()) > PAINT_SIZE {
        image.resize(
            PAINT_SIZE,
            PAINT_SIZE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        image
    }
    .to_rgba8();
    let (w, h) = (small.width() as usize, small.height() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let lumas: Vec<u8> = small
        .pixels()
        .map(|p| {
            (0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])).round()
                as u8
        })
        .collect();
    let mut histogram = [0usize; 256];
    for &l in &lumas {
        histogram[usize::from(l)] += 1;
    }
    let cut = lumas.len() / 50;
    // The first lightness level past the darkest (or lightest) 2 %.
    let past_cut = |levels: &[usize], from_top: bool| {
        let mut seen = 0;
        let mut past = |&n: &usize| {
            seen += n;
            seen > cut
        };
        let found = if from_top {
            levels.iter().rev().position(&mut past)
        } else {
            levels.iter().position(&mut past)
        };
        found.unwrap_or(0) as f32
    };
    let (mut lo, mut hi) = (
        past_cut(&histogram, false),
        255.0 - past_cut(&histogram, true),
    );
    if hi - lo < 24.0 {
        // A nearly flat cover: no stretching, or noise would become pattern.
        (lo, hi) = (0.0, 255.0);
    }
    let ramp = paint.ramp();
    let pixels = small
        .pixels()
        .zip(&lumas)
        .map(|(p, &l)| {
            let t = ((f32::from(l) - lo) / (hi - lo) * 255.0).clamp(0.0, 255.0) as usize;
            let c = ramp[t];
            Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), p[3])
        })
        .collect();
    Some(ColorImage::new([w, h], pixels))
}

/// What a derived URI asks for, and the cover it is made from.
fn job(uri: &str) -> Option<(Paint, &str)> {
    let rest = uri.strip_prefix(PAINT)?;
    let (key, url) = rest.split_once('/')?;
    Some((Paint::parse(key)?, url))
}

enum Slot {
    Pending,
    Ready(Arc<ColorImage>),
    Failed(String),
}

pub struct Loader {
    runtime: tokio::runtime::Handle,
    slots: Arc<Mutex<HashMap<String, Slot>>>,
    /// Two at a time: a page of covers being painted never crowds out the
    /// rest of the backend or the interface.
    permits: Arc<tokio::sync::Semaphore>,
}

impl Loader {
    pub fn new(runtime: tokio::runtime::Handle) -> Self {
        Self {
            runtime,
            slots: Arc::default(),
            permits: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }
}

impl ImageLoader for Loader {
    fn id(&self) -> &str {
        egui::generate_loader_id!(Loader)
    }

    fn load(&self, ctx: &Context, uri: &str, _: SizeHint) -> ImageLoadResult {
        let Some((job, url)) = job(uri) else {
            return Err(LoadError::NotSupported);
        };
        match self.slots.lock().expect("derived lock").get(uri) {
            Some(Slot::Ready(image)) => {
                return Ok(ImagePoll::Ready {
                    image: image.clone(),
                });
            }
            Some(Slot::Pending) => return Ok(ImagePoll::Pending { size: None }),
            Some(Slot::Failed(error)) => return Err(LoadError::Loading(error.clone())),
            None => {}
        }
        // The cover loader wakes the window when its bytes arrive.
        let bytes: Arc<[u8]> = match ctx.try_load_bytes(url)? {
            BytesPoll::Ready { bytes, .. } => match bytes {
                egui::load::Bytes::Shared(shared) => shared,
                egui::load::Bytes::Static(slice) => Arc::from(slice),
            },
            BytesPoll::Pending { .. } => return Ok(ImagePoll::Pending { size: None }),
        };
        self.slots
            .lock()
            .expect("derived lock")
            .insert(uri.to_owned(), Slot::Pending);
        let slots = self.slots.clone();
        let permits = self.permits.clone();
        let uri = uri.to_owned();
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let _permit = permits.acquire().await;
            let made = tokio::task::spawn_blocking(move || paint(&bytes, job))
                .await
                .ok()
                .flatten();
            let slot = match made {
                Some(image) => Slot::Ready(Arc::new(image)),
                None => Slot::Failed("the cover couldn't be read".into()),
            };
            slots.lock().expect("derived lock").insert(uri, slot);
            ctx.request_repaint();
        });
        Ok(ImagePoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        self.slots.lock().expect("derived lock").remove(uri);
    }

    fn forget_all(&self) {
        self.slots.lock().expect("derived lock").clear();
    }

    fn byte_size(&self) -> usize {
        self.slots
            .lock()
            .expect("derived lock")
            .values()
            .map(|s| match s {
                Slot::Ready(image) => image.pixels.len() * 4,
                _ => 0,
            })
            .sum()
    }

    fn has_pending(&self) -> bool {
        self.slots
            .lock()
            .expect("derived lock")
            .values()
            .any(|s| matches!(s, Slot::Pending))
    }
}

// ---- painted covers in the interface ----

/// Which painting covers are drawn in, kept in the window's memory.
#[derive(Clone, Default)]
struct PaintState {
    /// The current palette's colours, while painting is on.
    key: Option<String>,
    /// Now Playing and Stage are being drawn: covers there stay as they are.
    suppressed: bool,
    /// Covers asked for under `key` (to free them when it changes) and the
    /// ones of those drawn painted.
    asked: HashSet<String>,
    ready: HashSet<String>,
    /// The previous palette's painting, which stands in for a cover until
    /// its new painting is made, so a theme switch never blinks.
    previous: Option<(String, HashSet<String>, HashSet<String>)>,
    /// When `key` last changed, and when the previous painting last stood in.
    switched: f64,
    stood_in: f64,
}

fn state_id() -> Id {
    Id::new("ytfast-paint-state")
}

fn uris(key: &str, urls: &HashSet<String>) -> impl Iterator<Item = String> {
    urls.iter().map(move |u| paint_uri(key, u))
}

/// Called once a frame before drawing: painting on (`Some` palette) or off.
/// When the palette changes, the old painting is freed once every cover on
/// screen has its new one (or after a minute at most).
pub fn paint_frame(ctx: &Context, palette: Option<&Palette>) {
    let key = palette.map(|p| Paint::from_palette(p).key());
    let now = ctx.input(|i| i.time);
    let forget: Vec<String> = ctx.data_mut(|d| {
        let s = d.get_temp_mut_or_default::<PaintState>(state_id());
        s.suppressed = false;
        let mut forget = Vec::new();
        if s.key != key {
            if let Some((old, asked, _)) = s.previous.take() {
                forget.extend(uris(&old, &asked));
            }
            let asked = std::mem::take(&mut s.asked);
            let ready = std::mem::take(&mut s.ready);
            if let Some(old) = s.key.take() {
                if key.is_some() {
                    s.previous = Some((old, asked, ready));
                } else {
                    forget.extend(uris(&old, &asked));
                }
            }
            s.key = key;
            s.switched = now;
        } else if s.previous.is_some()
            && ((now - s.switched > 2.0 && now - s.stood_in > 2.0) || now - s.switched > 60.0)
            && let Some((old, asked, _)) = s.previous.take()
        {
            forget.extend(uris(&old, &asked));
        }
        forget
    });
    for uri in forget {
        ctx.forget_image(&uri);
    }
}

/// Runs `draw` with covers left unpainted: Now Playing and Stage always show
/// the real cover.
pub fn without_paint<R>(ctx: &Context, draw: impl FnOnce() -> R) -> R {
    let was = ctx.data_mut(|d| {
        let s = d.get_temp_mut_or_default::<PaintState>(state_id());
        std::mem::replace(&mut s.suppressed, true)
    });
    let result = draw();
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<PaintState>(state_id())
            .suppressed = was
    });
    result
}

/// The image to draw for the cover at `url`, `size` points large: its
/// painting while painting is on (the previous palette's, or the plain
/// cover, until the painting is made), else the cover itself.
pub fn cover_source<'a>(ctx: &Context, url: &'a str, size: Vec2) -> Cow<'a, str> {
    let (key, previous) = ctx.data_mut(|d| {
        let s = d.get_temp_mut_or_default::<PaintState>(state_id());
        if s.suppressed {
            return (None, None);
        }
        let previous = s
            .previous
            .as_ref()
            .filter(|(_, _, ready)| ready.contains(url))
            .map(|(k, _, _)| k.clone());
        (s.key.clone(), previous)
    });
    let Some(key) = key else {
        return Cow::Borrowed(url);
    };
    let uri = paint_uri(&key, url);
    let ready = matches!(
        egui::Image::new(uri.as_str()).load_for_size(ctx, size),
        Ok(egui::load::TexturePoll::Ready { .. })
    );
    let now = ctx.input(|i| i.time);
    ctx.data_mut(|d| {
        let s = d.get_temp_mut_or_default::<PaintState>(state_id());
        if s.key.as_deref() != Some(key.as_str()) {
            return;
        }
        if !s.asked.contains(url) {
            s.asked.insert(url.to_owned());
        }
        if ready {
            if !s.ready.contains(url) {
                s.ready.insert(url.to_owned());
            }
        } else if previous.is_some() {
            s.stood_in = now;
        }
    });
    match (ready, previous) {
        (true, _) => Cow::Owned(uri),
        (false, Some(old)) => Cow::Owned(paint_uri(&old, url)),
        (false, None) => Cow::Borrowed(url),
    }
}
