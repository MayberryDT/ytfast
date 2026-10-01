//! One ytfast at a time. A second launch asks the running one to show its
//! window (or reload themes, for the Omarchy hook) over a private socket in
//! the runtime directory, then exits.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    Show,
    ReloadThemes,
}

impl Message {
    fn word(self) -> &'static str {
        match self {
            Message::Show => "show",
            Message::ReloadThemes => "reload-themes",
        }
    }
}

/// Sends `message` to a running instance. `false` when none answers.
pub fn notify(runtime: &Path, message: Message) -> bool {
    let Ok(mut stream) = UnixStream::connect(runtime.join("ytfast.sock")) else {
        return false;
    };
    stream
        .write_all(format!("{}\n", message.word()).as_bytes())
        .is_ok()
}

/// Claims the socket and forwards messages from later launches.
pub fn listen(
    runtime: &Path,
    on_message: impl Fn(Message) + Send + 'static,
) -> std::io::Result<()> {
    let path = runtime.join("ytfast.sock");
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::thread::Builder::new()
        .name("ytfast-instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).is_ok() {
                    match line.trim() {
                        "show" => on_message(Message::Show),
                        "reload-themes" => on_message(Message::ReloadThemes),
                        _ => {}
                    }
                }
            }
        })?;
    Ok(())
}
