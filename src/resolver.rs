//! Turns a video id into a playable audio URL through yt-dlp.
//!
//! yt-dlp solves YouTube's JS challenges and, with the session's cookies,
//! reaches Premium's Opus ~256 kbps (itag 774). It takes several seconds per
//! track on the test machine (docs/plan/integration.md), so results are
//! cached until shortly before the URL expires, upcoming tracks are resolved
//! while the current one plays, and songs the pointer rests on are prepared
//! when nothing else is resolving. The iOS client's direct URLs were tried
//! and dropped: they stop after the first bytes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::innertube::Stream;

pub struct Resolver {
    /// Results by video id.
    cache: Mutex<HashMap<String, Stream>>,
    /// The session's cookies for yt-dlp; `None` when signed out.
    cookie_file: Mutex<Option<PathBuf>>,
    scratch: PathBuf,
    /// One yt-dlp at a time: it is CPU-heavy on small machines.
    ytdlp: tokio::sync::Mutex<()>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn describe(itag: u32) -> String {
    match itag {
        774 => "Opus 256 kbps · Premium (itag 774)".into(),
        141 => "AAC 256 kbps · Premium (itag 141)".into(),
        251 => "Opus 160 kbps (itag 251)".into(),
        140 => "AAC 128 kbps (itag 140)".into(),
        250 => "Opus 70 kbps (itag 250)".into(),
        249 => "Opus 50 kbps (itag 249)".into(),
        139 => "AAC 48 kbps (itag 139)".into(),
        other => format!("itag {other}"),
    }
}

impl Resolver {
    pub fn new(scratch: PathBuf) -> Self {
        Self {
            cache: Mutex::default(),
            cookie_file: Mutex::default(),
            scratch,
            ytdlp: tokio::sync::Mutex::new(()),
        }
    }

    pub fn set_cookie_file(&self, path: Option<PathBuf>) {
        *self.cookie_file.lock().expect("cookie lock") = path;
        self.cache.lock().expect("cache lock").clear();
    }

    /// A cached stream still valid for ten minutes.
    pub fn cached(&self, video_id: &str) -> Option<Stream> {
        let cache = self.cache.lock().expect("cache lock");
        cache
            .get(video_id)
            .filter(|s| s.expires > now() + 600)
            .cloned()
    }

    pub fn forget(&self, video_id: &str) {
        self.cache.lock().expect("cache lock").remove(video_id);
    }

    /// The best stream the account can get. Cached.
    pub async fn resolve(&self, video_id: &str) -> Result<Stream> {
        #[cfg(feature = "e2e")]
        if crate::e2e::sabotaged(video_id) {
            return Ok(Stream {
                itag: 251,
                url: "http://127.0.0.1:9/ytfast-e2e-broken".into(),
                user_agent: None,
                expires: now() + 3600,
            });
        }
        #[cfg(feature = "e2e")]
        if crate::e2e::offline() {
            bail!("Unable to reach YouTube (simulated offline)");
        }
        if let Some(stream) = self.cached(video_id) {
            return Ok(stream);
        }
        let _turn = self.ytdlp.lock().await;
        self.resolve_locked(video_id).await
    }

    /// Resolves ahead of a likely click, only when no other resolve is
    /// running or waiting, so it never delays real playback.
    pub async fn prepare(&self, video_id: &str) {
        if self.cached(video_id).is_some() {
            return;
        }
        let Ok(_turn) = self.ytdlp.try_lock() else {
            return;
        };
        if let Err(error) = self.resolve_locked(video_id).await {
            log::debug!("preparing a track failed: {error:#}");
        }
    }

    async fn resolve_locked(&self, video_id: &str) -> Result<Stream> {
        if let Some(stream) = self.cached(video_id) {
            return Ok(stream);
        }
        let started = std::time::Instant::now();
        let stream = self.run_ytdlp(video_id).await?;
        log::info!("resolved itag {} in {:?}", stream.itag, started.elapsed());
        self.cache
            .lock()
            .expect("cache lock")
            .insert(video_id.to_owned(), stream.clone());
        Ok(stream)
    }

    async fn run_ytdlp(&self, video_id: &str) -> Result<Stream> {
        let cookies = self.cookie_file.lock().expect("cookie lock").clone();
        // yt-dlp rewrites the cookie file it is given, so it gets a copy.
        let copy = cookies
            .as_ref()
            .map(|_| self.scratch.join(format!("ytdlp-{video_id}.txt")));
        if let (Some(from), Some(to)) = (&cookies, &copy) {
            std::fs::copy(from, to).context("copying cookies for yt-dlp")?;
        }
        let mut command = tokio::process::Command::new("yt-dlp");
        command.args([
            "--ignore-config",
            "--no-warnings",
            "--no-playlist",
            "-f",
            "774/141/251/140/250/249/139",
        ]);
        // yt-dlp's own client choice: forcing `web_music` stopped working for
        // this account on 2026-10-01 (it now needs a PO token and yields no audio).
        command.args([
            "--print",
            "%(format_id)s\t%(http_headers.User-Agent)s\t%(url)s",
        ]);
        if let Some(copy) = &copy {
            command.arg("--cookies").arg(copy);
        }
        command.arg(format!("https://music.youtube.com/watch?v={video_id}"));
        command
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null());
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(60), command.output()).await;
        if let Some(copy) = &copy {
            let _ = std::fs::remove_file(copy);
        }
        let output = output
            .context("yt-dlp timed out")?
            .context("running yt-dlp")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let line = stderr
                .lines()
                .rev()
                .find(|l| l.contains("ERROR"))
                .unwrap_or("yt-dlp failed");
            bail!("{}", line.trim());
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut parts = stdout.trim().splitn(3, '\t');
        let (Some(format), Some(agent), Some(url)) = (parts.next(), parts.next(), parts.next())
        else {
            bail!("yt-dlp printed no stream");
        };
        let itag = format
            .split('-')
            .next()
            .and_then(|f| f.parse().ok())
            .unwrap_or(0);
        Ok(Stream {
            itag,
            expires: crate::innertube::expiry(url),
            url: url.to_owned(),
            user_agent: (agent != "NA").then(|| agent.to_owned()),
        })
    }
}
