use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use anyhow::anyhow;
use ytfast::single_instance::{self, Message};

fn main() -> anyhow::Result<()> {
    let started = Instant::now();
    let paths = ytfast::paths::Paths::new()?;
    match std::env::args().nth(1).as_deref() {
        // The Omarchy theme hook's call: only a running instance cares.
        Some("reload-themes") => {
            single_instance::notify(&paths.runtime, Message::ReloadThemes);
            return Ok(());
        }
        Some(other) => {
            eprintln!("ytfast: unknown argument {other:?}\nusage: ytfast [reload-themes]");
            std::process::exit(2);
        }
        None => {}
    }
    if single_instance::notify(&paths.runtime, Message::Show) {
        return Ok(());
    }
    fastframe_log::Logging::new("ytfast", env!("CARGO_PKG_VERSION"))
        .filter("ytfast=info,warn")
        .file(paths.cache.join("ytfast.log"))
        .panic_log(paths.cache.join("panics.log"))
        .init()
        .map_err(|e| anyhow!("logging: {e}"))?;

    let ctx: Arc<OnceLock<egui::Context>> = Arc::default();
    let wake = {
        let ctx = ctx.clone();
        move || {
            if let Some(ctx) = ctx.get() {
                ctx.request_repaint();
            }
        }
    };
    let show = Arc::new(AtomicBool::new(false));
    let reload = Arc::new(AtomicBool::new(false));
    {
        let (show, reload, wake) = (show.clone(), reload.clone(), wake.clone());
        single_instance::listen(&paths.runtime, move |message| {
            match message {
                Message::Show => show.store(true, Ordering::Relaxed),
                Message::ReloadThemes => reload.store(true, Ordering::Relaxed),
            }
            wake();
        })?;
    }
    let backend = ytfast::backend::Backend::start(paths.clone(), wake)?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("ytfast")
            .with_title("Music")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ytfast",
        options,
        Box::new(move |cc| {
            let _ = ctx.set(cc.egui_ctx.clone());
            Ok(Box::new(ytfast::app::App::new(
                cc, backend, paths, show, reload, started,
            )))
        }),
    )
    .map_err(|e| anyhow!("{e}"))
}
