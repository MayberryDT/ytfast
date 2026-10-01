//! An audio-only mpv process controlled over its JSON IPC socket.
//!
//! mpv plays the resolved stream URLs; ytfast keeps at most the current and
//! the next track in mpv's playlist, so track changes are gapless.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Debug)]
pub enum MpvEvent {
    Property {
        name: String,
        data: Value,
    },
    /// A playlist entry finished: `eof`, `error`, `stop`, `quit` or `redirect`.
    EndFile {
        reason: String,
        entry: i64,
        error: Option<String>,
    },
    StartFile {
        entry: i64,
    },
    /// The process exited.
    Died,
}

pub struct Mpv {
    writer: Mutex<OwnedWriteHalf>,
    next_id: AtomicU64,
    pending: Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    _child: tokio::process::Child,
}

/// Properties whose changes are reported as [`MpvEvent::Property`].
const OBSERVED: &[&str] = &[
    "time-pos",
    "duration",
    "pause",
    "playlist-pos",
    "idle-active",
    "paused-for-cache",
    "volume",
    "seeking",
];

impl Mpv {
    pub async fn spawn(
        socket: &Path,
        volume: f64,
        events: mpsc::UnboundedSender<MpvEvent>,
    ) -> Result<Arc<Self>> {
        let _ = std::fs::remove_file(socket);
        let mut child = tokio::process::Command::new("mpv")
            .args([
                "--idle=yes",
                "--no-video",
                "--no-terminal",
                "--no-config",
                "--ytdl=no",
                "--gapless-audio=yes",
                "--prefetch-playlist=yes",
                "--cache=yes",
                "--demuxer-max-bytes=64MiB",
                "--audio-client-name=ytfast",
                "--replaygain=no",
            ])
            .arg(format!("--volume={volume}"))
            .arg(format!("--input-ipc-server={}", socket.display()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("starting mpv")?;
        let stream = connect(socket, &mut child).await?;
        let (reader, writer) = stream.into_split();
        let pending: Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>> = Arc::default();
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = message.get("request_id").and_then(Value::as_u64) {
                    if let Some(sender) = pending_reader.lock().expect("pending lock").remove(&id) {
                        let _ = sender.send(message);
                    }
                    continue;
                }
                let event = match message.get("event").and_then(Value::as_str) {
                    Some("property-change") => MpvEvent::Property {
                        name: message
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        data: message.get("data").cloned().unwrap_or(Value::Null),
                    },
                    Some("end-file") => MpvEvent::EndFile {
                        reason: message
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        entry: message
                            .get("playlist_entry_id")
                            .and_then(Value::as_i64)
                            .unwrap_or(-1),
                        error: message
                            .get("file_error")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    },
                    Some("start-file") => MpvEvent::StartFile {
                        entry: message
                            .get("playlist_entry_id")
                            .and_then(Value::as_i64)
                            .unwrap_or(-1),
                    },
                    _ => continue,
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            let _ = events.send(MpvEvent::Died);
        });
        let mpv = Arc::new(Self {
            writer: Mutex::new(writer),
            next_id: AtomicU64::new(1),
            pending,
            _child: child,
        });
        for (i, name) in OBSERVED.iter().enumerate() {
            mpv.command(json!(["observe_property", i + 1, name]))
                .await?;
        }
        Ok(mpv)
    }

    /// Runs a command and returns its `data`.
    pub async fn command(&self, args: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending lock")
            .insert(id, sender);
        let mut line = serde_json::to_vec(&json!({ "command": args, "request_id": id }))?;
        line.push(b'\n');
        self.writer
            .lock()
            .await
            .write_all(&line)
            .await
            .context("writing to mpv")?;
        let reply = tokio::time::timeout(Duration::from_secs(5), receiver)
            .await
            .map_err(|_| anyhow!("mpv did not answer"))?
            .map_err(|_| anyhow!("mpv closed"))?;
        match reply.get("error").and_then(Value::as_str) {
            Some("success") => Ok(reply.get("data").cloned().unwrap_or(Value::Null)),
            Some(error) => bail!("mpv: {error}"),
            None => bail!("mpv: malformed reply"),
        }
    }

    pub async fn set(&self, property: &str, value: Value) -> Result<()> {
        self.command(json!(["set_property", property, value]))
            .await
            .map(|_| ())
    }

    pub async fn get(&self, property: &str) -> Result<Value> {
        self.command(json!(["get_property", property])).await
    }

    /// Loads `url` (`replace` or `append`) with per-file options: the
    /// request headers the stream needs, its loudness gain, a start time.
    /// mpv applies them when the file starts and restores them when it ends,
    /// so each holds for its own song, gapless handoff included.
    pub async fn load(&self, url: &str, mode: &str, options: &[(&str, String)]) -> Result<i64> {
        // mpv's option lists split on commas; `%N%value` quotes a value of N bytes.
        let options = options
            .iter()
            .map(|(name, value)| format!("{name}=%{}%{value}", value.len()))
            .collect::<Vec<_>>()
            .join(",");
        let data = self
            .command(json!(["loadfile", url, mode, -1, options]))
            .await?;
        Ok(data
            .get("playlist_entry_id")
            .and_then(Value::as_i64)
            .unwrap_or(-1))
    }
}

async fn connect(socket: &Path, child: &mut tokio::process::Child) -> Result<UnixStream> {
    let path = PathBuf::from(socket);
    for _ in 0..250 {
        if let Ok(stream) = UnixStream::connect(&path).await {
            return Ok(stream);
        }
        if let Ok(Some(status)) = child.try_wait() {
            bail!("mpv exited ({status})");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    bail!("mpv's control socket did not appear")
}
