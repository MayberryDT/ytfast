//! Cover art for egui: an `https://` bytes loader with a disk cache, so the
//! covers of the last session appear at once on the next launch.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::load::{Bytes, BytesLoadResult, BytesLoader, BytesPoll, LoadError};

use crate::paths::Paths;

enum Slot {
    Pending,
    Ready(Arc<[u8]>, Option<String>, std::time::Instant),
    /// Failed at this time; tried again after a while (the connection may be back).
    Failed(String, std::time::Instant),
}

pub struct CoverLoader {
    runtime: tokio::runtime::Handle,
    http: reqwest::Client,
    paths: Paths,
    slots: Arc<Mutex<HashMap<String, Slot>>>,
    /// Bounds concurrent downloads.
    permits: Arc<tokio::sync::Semaphore>,
}

impl CoverLoader {
    pub fn new(runtime: tokio::runtime::Handle, http: reqwest::Client, paths: Paths) -> Self {
        Self {
            runtime,
            http,
            paths,
            slots: Arc::default(),
            permits: Arc::new(tokio::sync::Semaphore::new(6)),
        }
    }
}

fn mime(bytes: &[u8]) -> Option<String> {
    let kind = if bytes.starts_with(&[0xFF, 0xD8]) {
        "image/jpeg"
    } else if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        return None;
    };
    Some(kind.to_owned())
}

impl BytesLoader for CoverLoader {
    fn id(&self) -> &str {
        egui::generate_loader_id!(CoverLoader)
    }

    fn load(&self, ctx: &egui::Context, uri: &str) -> BytesLoadResult {
        if !uri.starts_with("https://") {
            return Err(LoadError::NotSupported);
        }
        let mut slots = self.slots.lock().expect("covers lock");
        match slots.get_mut(uri) {
            Some(Slot::Ready(bytes, mime, used)) => {
                *used = std::time::Instant::now();
                return Ok(BytesPoll::Ready {
                    size: None,
                    bytes: Bytes::Shared(bytes.clone()),
                    mime: mime.clone(),
                });
            }
            Some(Slot::Pending) => return Ok(BytesPoll::Pending { size: None }),
            Some(Slot::Failed(error, at)) if at.elapsed() < std::time::Duration::from_secs(30) => {
                return Err(LoadError::Loading(error.clone()));
            }
            Some(Slot::Failed(..)) => {}
            None => {}
        }
        slots.insert(uri.to_owned(), Slot::Pending);
        drop(slots);
        let uri = uri.to_owned();
        let file = self.paths.cover_file(&uri);
        let http = self.http.clone();
        let slots = self.slots.clone();
        let permits = self.permits.clone();
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let result = match tokio::fs::read(&file).await {
                Ok(bytes) if !bytes.is_empty() => Ok(bytes),
                _ => {
                    let _permit = permits.acquire().await;
                    match http
                        .get(&uri)
                        .send()
                        .await
                        .and_then(reqwest::Response::error_for_status)
                    {
                        Ok(response) => match response.bytes().await {
                            Ok(bytes) => {
                                let _ = crate::paths::write_atomic(&file, &bytes);
                                Ok(bytes.to_vec())
                            }
                            Err(error) => Err(error.to_string()),
                        },
                        Err(error) => Err(error.to_string()),
                    }
                }
            };
            let slot = match result {
                Ok(bytes) => {
                    let mime = mime(&bytes);
                    Slot::Ready(Arc::from(bytes), mime, std::time::Instant::now())
                }
                Err(error) => Slot::Failed(error, std::time::Instant::now()),
            };
            let mut held = slots.lock().expect("covers lock");
            if held.contains_key(&uri) {
                held.insert(uri, slot);
            }
            let mut total: usize = held
                .values()
                .map(|s| match s {
                    Slot::Ready(b, _, _) => b.len(),
                    _ => 0,
                })
                .sum();
            if total > 16 * 1024 * 1024 {
                let mut keys: Vec<_> = held
                    .iter()
                    .filter_map(|(k, s)| match s {
                        Slot::Ready(_, _, used) => Some((k.clone(), *used)),
                        _ => None,
                    })
                    .collect();
                keys.sort_by_key(|(_, used)| *used);
                for (key, _) in keys {
                    if total <= 16 * 1024 * 1024 {
                        break;
                    }
                    if let Some(Slot::Ready(b, _, _)) = held.remove(&key) {
                        total = total.saturating_sub(b.len());
                    }
                }
            }
            drop(held);
            ctx.request_repaint();
        });
        Ok(BytesPoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        self.slots.lock().expect("covers lock").remove(uri);
    }

    fn forget_all(&self) {
        self.slots.lock().expect("covers lock").clear();
    }

    fn byte_size(&self) -> usize {
        self.slots
            .lock()
            .expect("covers lock")
            .values()
            .map(|s| match s {
                Slot::Ready(bytes, _, _) => bytes.len(),
                _ => 0,
            })
            .sum()
    }

    fn has_pending(&self) -> bool {
        self.slots
            .lock()
            .expect("covers lock")
            .values()
            .any(|s| matches!(s, Slot::Pending))
    }
}

// Artwork is decoded once per size bucket, off the UI thread. The frame
// tracker accounts for GPU textures too; encoded byte counts alone miss them.
const SCALED: &str = "ytfast-cover://";
const BUDGET: usize = 64 * 1024 * 1024;
#[derive(Clone, Default)]
struct Usage(std::collections::HashMap<String, (u64, usize)>);
fn usage_id() -> egui::Id {
    egui::Id::new("artwork-usage")
}
pub fn source(ctx: &egui::Context, url: &str, size: egui::Vec2) -> String {
    let pixels = (size.max_elem() * ctx.pixels_per_point())
        .ceil()
        .clamp(32.0, 2048.0) as u32;
    let pixels = pixels.div_ceil(32) * 32;
    format!("{SCALED}{pixels}/{url}")
}
pub fn touch(ctx: &egui::Context, uri: &str, size: egui::Vec2) {
    let pixels = (size.max_elem() * ctx.pixels_per_point())
        .ceil()
        .clamp(32.0, 2048.0) as usize;
    let side = pixels.div_ceil(32) * 32;
    let frame = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Usage>(usage_id())
            .0
            .insert(uri.to_owned(), (frame, side * side * 4));
    });
}
pub fn finish_frame(ctx: &egui::Context) {
    let current_frame = ctx.cumulative_pass_nr();
    let forget = ctx.data_mut(|d| {
        let usage = &mut d.get_temp_mut_or_default::<Usage>(usage_id()).0;
        let mut total: usize = usage.values().map(|(_, bytes)| bytes).sum();
        let mut oldest: Vec<_> = usage
            .iter()
            .map(|(uri, (frame, bytes))| (uri.clone(), *frame, *bytes))
            .collect();
        oldest.sort_by_key(|(_, frame, _)| *frame);
        let mut forget = Vec::new();
        for (uri, frame, bytes) in oldest {
            if total <= BUDGET {
                break;
            }
            // Keep visible art stable even when one frame exceeds the budget.
            if frame == current_frame {
                continue;
            }
            total = total.saturating_sub(bytes);
            usage.remove(&uri);
            forget.push(uri);
        }
        forget
    });
    for uri in forget {
        ctx.forget_image(&uri);
    }
}

enum ImageSlot {
    Pending,
    Ready(Arc<egui::ColorImage>),
    Failed(String),
}
pub struct ScaledLoader {
    runtime: tokio::runtime::Handle,
    slots: Arc<Mutex<HashMap<String, ImageSlot>>>,
}
impl ScaledLoader {
    pub fn new(runtime: tokio::runtime::Handle) -> Self {
        Self {
            runtime,
            slots: Arc::default(),
        }
    }
}
impl egui::load::ImageLoader for ScaledLoader {
    fn id(&self) -> &str {
        egui::generate_loader_id!(ScaledLoader)
    }
    fn load(
        &self,
        ctx: &egui::Context,
        uri: &str,
        _: egui::load::SizeHint,
    ) -> egui::load::ImageLoadResult {
        use egui::load::ImagePoll;
        let Some(rest) = uri.strip_prefix(SCALED) else {
            return Err(LoadError::NotSupported);
        };
        let (pixels, url) = rest.split_once('/').ok_or(LoadError::NotSupported)?;
        let pixels: u32 = pixels.parse().map_err(|_| LoadError::NotSupported)?;
        if !(32..=2048).contains(&pixels) {
            return Err(LoadError::NotSupported);
        }
        let mut slots = self.slots.lock().expect("scaled covers lock");
        match slots.get(uri) {
            Some(ImageSlot::Ready(image)) => {
                return Ok(ImagePoll::Ready {
                    image: image.clone(),
                });
            }
            Some(ImageSlot::Pending) => return Ok(ImagePoll::Pending { size: None }),
            Some(ImageSlot::Failed(error)) => return Err(LoadError::Loading(error.clone())),
            None => {}
        }
        let bytes = match ctx.try_load_bytes(url)? {
            BytesPoll::Pending { .. } => return Ok(ImagePoll::Pending { size: None }),
            BytesPoll::Ready { bytes, .. } => bytes,
        };
        slots.insert(uri.to_owned(), ImageSlot::Pending);
        drop(slots);
        let slots = self.slots.clone();
        let ctx = ctx.clone();
        let uri = uri.to_owned();
        self.runtime.spawn_blocking(move || {
            let image = image::load_from_memory(bytes.as_ref()).map(|image| {
                let small = image.thumbnail(pixels, pixels).to_rgba8();
                egui::ColorImage::from_rgba_unmultiplied(
                    [small.width() as usize, small.height() as usize],
                    small.as_raw(),
                )
            });
            let slot = match image {
                Ok(image) => ImageSlot::Ready(Arc::new(image)),
                Err(e) => ImageSlot::Failed(e.to_string()),
            };
            let mut slots = slots.lock().expect("scaled covers lock");
            // An evicted in-flight image must not resurrect itself.
            if slots.contains_key(&uri) {
                slots.insert(uri, slot);
            }
            drop(slots);
            ctx.request_repaint();
        });
        Ok(ImagePoll::Pending { size: None })
    }
    fn forget(&self, uri: &str) {
        self.slots.lock().expect("scaled covers lock").remove(uri);
    }
    fn forget_all(&self) {
        self.slots.lock().expect("scaled covers lock").clear();
    }
    fn byte_size(&self) -> usize {
        self.slots
            .lock()
            .expect("scaled covers lock")
            .values()
            .map(|slot| match slot {
                ImageSlot::Ready(image) => image.pixels.len() * 4,
                _ => 0,
            })
            .sum()
    }
    fn has_pending(&self) -> bool {
        self.slots
            .lock()
            .expect("scaled covers lock")
            .values()
            .any(|s| matches!(s, ImageSlot::Pending))
    }
}
