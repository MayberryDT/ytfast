//! Cover art for egui: an `https://` bytes loader with a disk cache, so the
//! covers of the last session appear at once on the next launch.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::load::{Bytes, BytesLoadResult, BytesLoader, BytesPoll, LoadError};

use crate::paths::Paths;

enum Slot {
    Pending,
    Ready(Arc<[u8]>, Option<String>),
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
        match slots.get(uri) {
            Some(Slot::Ready(bytes, mime)) => {
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
                    Slot::Ready(Arc::from(bytes), mime)
                }
                Err(error) => Slot::Failed(error, std::time::Instant::now()),
            };
            slots.lock().expect("covers lock").insert(uri, slot);
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
                Slot::Ready(bytes, _) => bytes.len(),
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
