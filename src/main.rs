use std::sync::Arc;
use std::time::Instant;

use anyhow::anyhow;
use ytfast::app::{App, Window, WindowKind};
use ytfast::desktop::{Flags, Remote, Request};
use ytfast::single_instance::{self, Message};

const USAGE: &str = "usage: ytfast [command]

Without a command, opens Music (or brings back the running one).

  show               bring back the window
  toggle             play or pause
  play | pause
  next | previous
  like               like or unlike the playing song
  open <link>        open a YouTube Music or YouTube link (starts Music if needed)
  quit               quit Music, stopping playback
  reload-themes      reload the Omarchy theme (the theme hook uses this)";

fn main() -> anyhow::Result<()> {
    let started = Instant::now();
    let paths = ytfast::paths::Paths::new()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    // A link to open once the app runs (`ytfast open <link>` with none running).
    let mut link = None;
    match args.first().map(String::as_str) {
        None => {
            if single_instance::notify(&paths.runtime, &Message::Show) {
                return Ok(());
            }
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some(word) => {
            let Some(message) = Message::parse(word, args.get(1).map(String::as_str)) else {
                eprintln!("ytfast: unknown command {:?}\n{USAGE}", args.join(" "));
                std::process::exit(2);
            };
            if let Message::Open(target) = &message
                && ytfast::links::target_from_link(target).is_none()
            {
                eprintln!("ytfast: not a YouTube Music or YouTube link: {target}");
                std::process::exit(2);
            }
            if single_instance::notify(&paths.runtime, &message) {
                return Ok(());
            }
            match message {
                // Nothing running: start, and open the link or just show.
                Message::Open(target) => link = Some(target),
                Message::Show => {}
                // Only a running instance cares about the theme hook.
                Message::ReloadThemes => return Ok(()),
                _ => {
                    eprintln!("ytfast: Music isn't running");
                    std::process::exit(1);
                }
            }
        }
    }
    fastframe_log::Logging::new("ytfast", env!("CARGO_PKG_VERSION"))
        .filter("ytfast=info,warn")
        .file(paths.cache.join("ytfast.log"))
        .panic_log(paths.cache.join("panics.log"))
        .init()
        .map_err(|e| anyhow!("logging: {e}"))?;

    // Repaints whichever window is open; the backend, MPRIS and the
    // instance socket outlive any window.
    let waker = fastframe_shell::Waker::default();
    let backend = {
        let waker = waker.clone();
        ytfast::backend::Backend::start(paths.clone(), move || waker.wake())?
    };
    let flags = Arc::new(Flags::default());
    flags.notifications.store(
        ytfast::settings::Settings::load(&paths).notifications,
        std::sync::atomic::Ordering::Relaxed,
    );
    let (request_tx, requests) = std::sync::mpsc::channel();
    if let Some(link) = link {
        let _ = request_tx.send(Request::Open(link));
    }
    let remote = Remote::new(
        backend.commands(),
        backend.now.clone(),
        request_tx.clone(),
        waker.clone(),
    );
    {
        let remote = remote.clone();
        single_instance::listen(&paths.runtime, move |message| remote.deliver(message))?;
    }
    ytfast::tray::start(
        &backend.runtime,
        remote.clone(),
        backend.now.clone(),
        flags.window_open.subscribe(),
    );
    ytfast::mpris::start(
        &backend.runtime,
        remote,
        backend.now.clone(),
        flags.clone(),
        paths.clone(),
        backend.http.clone(),
    );
    let app = App::new(
        backend,
        paths,
        flags,
        (request_tx, requests),
        waker.clone(),
        started,
    );
    fastframe_shell::Shell::new(app, &waker)
        .run(|lease| {
            let kind = lease.peek(|app| app.window);
            let (name, options) = native_options(kind);
            eframe::run_native(
                name,
                options,
                Box::new(move |cc| {
                    let mut app = lease.take(&cc.egui_ctx);
                    app.attach(&cc.egui_ctx);
                    Ok(Box::new(Window(app)))
                }),
            )
        })
        .map_err(|e| anyhow!("{e}"))
}

/// Each kind of window keeps its own saved size (eframe stores it under the name).
fn native_options(kind: WindowKind) -> (&'static str, eframe::NativeOptions) {
    let viewport = match kind {
        WindowKind::Main => egui::ViewportBuilder::default()
            .with_app_id("ytfast")
            .with_title("Music")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 600.0]),
        WindowKind::Mini => egui::ViewportBuilder::default()
            .with_app_id("ytfast-mini")
            .with_title("Music")
            .with_inner_size(ytfast::ui::mini::SIZE)
            .with_min_inner_size([320.0, 120.0]),
    };
    let name = match kind {
        WindowKind::Main => "ytfast",
        WindowKind::Mini => "ytfast-mini",
    };
    (
        name,
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
    )
}
