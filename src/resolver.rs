//! Turns a video id into a playable audio URL through yt-dlp.
//!
//! yt-dlp solves YouTube's JS challenges and, with the session's cookies,
//! reaches Premium's Opus ~256 kbps (itag 774). A run takes ~4 s on the test
//! machine whatever is tried, but runs scale: three at once finish in ~4.9 s
//! (docs/integration.md). So resolves run in parallel at two priorities.
//! Playback (the current song, then the next one) has its own slots and
//! never waits behind speculation; speculation (songs on screen, under the
//! pointer, further ahead in the queue) has two more slots, runs niced, and
//! keeps a short most-likely-first backlog. A song asked for twice shares
//! one run, and a playback run nobody waits for any more is stopped.
//! Results are cached until ten minutes before the URL expires, in memory
//! and in the runtime directory (0600), so a relaunch can start at once.
//! The iOS client's direct URLs were tried and dropped: they stop after the
//! first bytes.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

use crate::innertube::Stream;

/// Runs at once for playback: a click can start while the song before it
/// still resolves.
const PLAYBACK_SLOTS: usize = 2;
const SPECULATIVE_SLOTS: usize = 2;
/// Guesses waiting for a speculative slot; older ones fall off.
const BACKLOG: usize = 8;
/// A cached URL is used until this many seconds before it expires.
const MARGIN: u64 = 600;

#[derive(Clone, Serialize, Deserialize)]
struct Cached {
    itag: u32,
    url: String,
    user_agent: Option<String>,
    expires: u64,
    /// Resolved with the account's cookies, so with its formats.
    signed_in: bool,
}

impl Cached {
    fn stream(&self) -> Stream {
        Stream {
            itag: self.itag,
            url: self.url.clone(),
            user_agent: self.user_agent.clone(),
            expires: self.expires,
        }
    }
}

type Outcome = Option<Result<Stream, String>>;

/// One yt-dlp run, shared by everyone who wants its song.
struct Flight {
    result: watch::Sender<Outcome>,
    waiters: AtomicUsize,
    /// Started for playback: stopped when nobody waits for it any more.
    cancellable: AtomicBool,
    abort: Mutex<Option<tokio::task::AbortHandle>>,
}

pub struct Resolver {
    cache: Mutex<HashMap<String, Cached>>,
    /// The session's cookies for yt-dlp; `None` when signed out.
    cookie_file: Mutex<Option<PathBuf>>,
    /// The runtime directory: cookie copies and the saved cache.
    scratch: PathBuf,
    flights: Mutex<HashMap<String, Arc<Flight>>>,
    playback: Arc<Semaphore>,
    speculative: Arc<Semaphore>,
    backlog: Mutex<VecDeque<String>>,
    /// Serialises writes of the saved cache.
    saving: Mutex<()>,
    runs: AtomicU64,
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

/// A resolve asked for: known at once, or a share of a run.
pub enum Request {
    Ready(Result<Stream>),
    Waiting(Waiting),
}

/// A share of a run; dropping the last share of a playback run stops it.
pub struct Waiting {
    resolver: Arc<Resolver>,
    id: String,
    flight: Arc<Flight>,
}

impl Request {
    pub async fn wait(self) -> Result<Stream> {
        let waiting = match self {
            Request::Ready(result) => return result,
            Request::Waiting(waiting) => waiting,
        };
        let mut rx = waiting.flight.result.subscribe();
        let outcome = rx
            .wait_for(Option::is_some)
            .await
            .map(|o| o.clone())
            .map_err(|_| anyhow!("the stream lookup stopped"))?;
        drop(waiting);
        outcome
            .unwrap_or_else(|| Err("the stream lookup stopped".into()))
            .map_err(|e| anyhow!(e))
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let abort = {
            let mut flights = self.resolver.flights.lock().expect("flights lock");
            let last = self.flight.waiters.fetch_sub(1, Ordering::SeqCst) == 1;
            if !(last
                && self.flight.cancellable.load(Ordering::SeqCst)
                && self.flight.result.borrow().is_none())
            {
                return;
            }
            if flights
                .get(&self.id)
                .is_some_and(|f| Arc::ptr_eq(f, &self.flight))
            {
                flights.remove(&self.id);
            }
            self.flight.abort.lock().expect("abort lock").take()
        };
        if let Some(abort) = abort {
            abort.abort();
            log::info!("stopped resolving {}: no longer wanted", self.id);
        }
    }
}

/// Removes a finished or stopped run from the table.
struct Landing {
    resolver: Arc<Resolver>,
    id: String,
    flight: Arc<Flight>,
}

impl Drop for Landing {
    fn drop(&mut self) {
        // A run that ends without an answer (stopped, or panicked) says so.
        if self.flight.result.borrow().is_none() {
            self.flight
                .result
                .send_replace(Some(Err("the stream lookup stopped".into())));
        }
        let mut flights = self.resolver.flights.lock().expect("flights lock");
        if flights
            .get(&self.id)
            .is_some_and(|f| Arc::ptr_eq(f, &self.flight))
        {
            flights.remove(&self.id);
        }
    }
}

/// yt-dlp's private copy of the cookie file, removed however the run ends.
struct CookieCopy(PathBuf);

impl Drop for CookieCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Resolver {
    /// Starts with the streams saved by an earlier run that are still valid.
    pub fn new(scratch: PathBuf) -> Self {
        let deadline = now() + MARGIN;
        let saved: HashMap<String, Cached> = std::fs::read(scratch.join("streams.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let cache: HashMap<String, Cached> = saved
            .into_iter()
            .filter(|(_, c)| c.expires > deadline)
            .collect();
        if !cache.is_empty() {
            log::info!("{} saved streams still valid", cache.len());
        }
        Self {
            cache: Mutex::new(cache),
            cookie_file: Mutex::default(),
            scratch,
            flights: Mutex::default(),
            playback: Arc::new(Semaphore::new(PLAYBACK_SLOTS)),
            speculative: Arc::new(Semaphore::new(SPECULATIVE_SLOTS)),
            backlog: Mutex::default(),
            saving: Mutex::default(),
            runs: AtomicU64::new(0),
        }
    }

    /// Streams resolved without cookies don't count once signed in (they
    /// lack the account's formats); the others stay usable whatever happens
    /// to the session, since stream URLs carry no cookies.
    pub fn set_cookie_file(&self, path: Option<PathBuf>) {
        *self.cookie_file.lock().expect("cookie lock") = path;
    }

    fn signed_in(&self) -> bool {
        self.cookie_file.lock().expect("cookie lock").is_some()
    }

    /// A cached stream still valid for ten minutes.
    pub fn cached(&self, video_id: &str) -> Option<Stream> {
        let signed_in = self.signed_in();
        let cache = self.cache.lock().expect("cache lock");
        cache
            .get(video_id)
            .filter(|c| c.expires > now() + MARGIN && (c.signed_in || !signed_in))
            .map(Cached::stream)
    }

    pub fn forget(&self, video_id: &str) {
        self.cache.lock().expect("cache lock").remove(video_id);
        self.save();
    }

    /// The best stream the account can get, for playback.
    pub async fn resolve(self: &Arc<Self>, video_id: &str) -> Result<Stream> {
        self.request(video_id).wait().await
    }

    /// Asks for a stream for playback. The share is taken at once, so a run
    /// handed from one waiter to the next is never stopped in between.
    pub fn request(self: &Arc<Self>, video_id: &str) -> Request {
        #[cfg(feature = "e2e")]
        if crate::e2e::sabotaged(video_id) {
            return Request::Ready(Ok(Stream {
                itag: 251,
                url: "http://127.0.0.1:9/ytfast-e2e-broken".into(),
                user_agent: None,
                expires: now() + 3600,
            }));
        }
        #[cfg(feature = "e2e")]
        if crate::e2e::offline() {
            return Request::Ready(Err(anyhow!("Unable to reach YouTube (simulated offline)")));
        }
        let mut flights = self.flights.lock().expect("flights lock");
        if let Some(stream) = self.cached(video_id) {
            return Request::Ready(Ok(stream));
        }
        let flight = match flights.get(video_id) {
            Some(flight) => flight.clone(),
            None => self.launch(&mut flights, video_id, None),
        };
        flight.waiters.fetch_add(1, Ordering::SeqCst);
        Request::Waiting(Waiting {
            resolver: self.clone(),
            id: video_id.to_owned(),
            flight,
        })
    }

    /// Resolves likely songs ahead of a click, most likely first, without
    /// taking a playback slot.
    pub fn prepare_many(self: &Arc<Self>, mut video_ids: Vec<String>) {
        #[cfg(feature = "e2e")]
        if crate::e2e::offline() {
            return;
        }
        video_ids.retain(|id| self.cached(id).is_none());
        if video_ids.is_empty() {
            return;
        }
        {
            // A playback run that is also a good guess keeps going if playback moves on.
            let flights = self.flights.lock().expect("flights lock");
            for id in &video_ids {
                if let Some(flight) = flights.get(id) {
                    flight.cancellable.store(false, Ordering::SeqCst);
                }
            }
        }
        {
            let mut backlog = self.backlog.lock().expect("backlog lock");
            backlog.retain(|id| !video_ids.contains(id));
            for id in video_ids.into_iter().rev() {
                backlog.push_front(id);
            }
            backlog.truncate(BACKLOG);
        }
        self.pump();
    }

    pub fn prepare(self: &Arc<Self>, video_id: &str) {
        self.prepare_many(vec![video_id.to_owned()]);
    }

    /// Waits for a song through the speculative path: prepared ahead (most
    /// likely first), sharing a run already under way, never taking a
    /// playback slot. For previews (Audition), which must not delay playback.
    pub async fn prepared(self: &Arc<Self>, video_id: &str) -> Result<Stream> {
        #[cfg(feature = "e2e")]
        if crate::e2e::offline() {
            bail!("Unable to reach YouTube (simulated offline)");
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(90);
        loop {
            if let Some(stream) = self.cached(video_id) {
                return Ok(stream);
            }
            let share = {
                let flights = self.flights.lock().expect("flights lock");
                flights.get(video_id).cloned().map(|flight| {
                    flight.waiters.fetch_add(1, Ordering::SeqCst);
                    Waiting {
                        resolver: self.clone(),
                        id: video_id.to_owned(),
                        flight,
                    }
                })
            };
            if let Some(share) = share {
                return Request::Waiting(share).wait().await;
            }
            if Instant::now() > deadline {
                bail!("the stream lookup took too long");
            }
            // Still waiting for a speculative slot: stay first in line.
            self.prepare(video_id);
            if !self
                .flights
                .lock()
                .expect("flights lock")
                .contains_key(video_id)
            {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }

    /// Starts guesses from the backlog while speculative slots are free.
    fn pump(self: &Arc<Self>) {
        loop {
            let Ok(permit) = self.speculative.clone().try_acquire_owned() else {
                return;
            };
            let mut flights = self.flights.lock().expect("flights lock");
            let next = loop {
                let Some(id) = self.backlog.lock().expect("backlog lock").pop_front() else {
                    break None;
                };
                if !flights.contains_key(&id) && self.cached(&id).is_none() {
                    break Some(id);
                }
            };
            let Some(id) = next else { return };
            self.launch(&mut flights, &id, Some(permit));
        }
    }

    /// Starts a run: speculative with a slot in hand, otherwise for playback
    /// once a playback slot is free.
    fn launch(
        self: &Arc<Self>,
        flights: &mut HashMap<String, Arc<Flight>>,
        video_id: &str,
        permit: Option<OwnedSemaphorePermit>,
    ) -> Arc<Flight> {
        let speculative = permit.is_some();
        let flight = Arc::new(Flight {
            result: watch::Sender::new(None),
            waiters: AtomicUsize::new(0),
            cancellable: AtomicBool::new(!speculative),
            abort: Mutex::default(),
        });
        flights.insert(video_id.to_owned(), flight.clone());
        let this = self.clone();
        let id = video_id.to_owned();
        let shared = flight.clone();
        let task = tokio::spawn(async move {
            let _landing = Landing {
                resolver: this.clone(),
                id: id.clone(),
                flight: shared.clone(),
            };
            let queued = Instant::now();
            let permit = match permit {
                Some(permit) => permit,
                None => match this.playback.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => return,
                },
            };
            let waited = queued.elapsed();
            let started = Instant::now();
            let result = this.run_ytdlp(&id, speculative).await;
            let kind = if speculative { "ahead" } else { "for playback" };
            match &result {
                Ok((stream, _)) => log::info!(
                    "resolved {id} {kind}: itag {} in {:.1}s (waited {:.1}s for a slot)",
                    stream.itag,
                    started.elapsed().as_secs_f64(),
                    waited.as_secs_f64()
                ),
                Err(error) => log::warn!(
                    "resolving {id} {kind} failed after {:.1}s: {error:#}",
                    started.elapsed().as_secs_f64()
                ),
            }
            let outcome = match result {
                Ok((stream, signed_in)) => {
                    this.store(&id, &stream, signed_in);
                    Ok(stream)
                }
                Err(error) => Err(format!("{error:#}")),
            };
            shared.result.send_replace(Some(outcome));
            drop(permit);
            if speculative {
                this.pump();
            }
        });
        *flight.abort.lock().expect("abort lock") = Some(task.abort_handle());
        flight
    }

    fn store(&self, video_id: &str, stream: &Stream, signed_in: bool) {
        self.cache.lock().expect("cache lock").insert(
            video_id.to_owned(),
            Cached {
                itag: stream.itag,
                url: stream.url.clone(),
                user_agent: stream.user_agent.clone(),
                expires: stream.expires,
                signed_in,
            },
        );
        self.save();
    }

    /// Writes the valid streams to the runtime directory, readable only by
    /// the user (the URLs are tied to the account).
    fn save(&self) {
        let _turn = self.saving.lock().expect("saving lock");
        let bytes = {
            let cache = self.cache.lock().expect("cache lock");
            let deadline = now() + MARGIN;
            let valid: HashMap<&String, &Cached> =
                cache.iter().filter(|(_, c)| c.expires > deadline).collect();
            match serde_json::to_vec(&valid) {
                Ok(bytes) => bytes,
                Err(_) => return,
            }
        };
        let path = self.scratch.join("streams.json");
        let temporary = self
            .scratch
            .join(format!("streams.json.tmp{}", std::process::id()));
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .and_then(|mut file| file.write_all(&bytes))
            .and_then(|()| std::fs::rename(&temporary, &path));
        if let Err(error) = written {
            log::warn!("couldn't save resolved streams: {error}");
        }
    }

    /// One yt-dlp run; also says whether it had the account's cookies.
    async fn run_ytdlp(&self, video_id: &str, speculative: bool) -> Result<(Stream, bool)> {
        let cookies = self.cookie_file.lock().expect("cookie lock").clone();
        // yt-dlp rewrites the cookie file it is given, so it gets a copy.
        let copy = cookies.as_ref().map(|_| {
            let run = self.runs.fetch_add(1, Ordering::Relaxed);
            CookieCopy(self.scratch.join(format!("ytdlp-{video_id}-{run}.txt")))
        });
        if let (Some(from), Some(to)) = (&cookies, &copy) {
            std::fs::copy(from, &to.0).context("copying cookies for yt-dlp")?;
        }
        // Guesses yield the CPU to playback's runs.
        let mut command = if speculative {
            let mut nice = tokio::process::Command::new("nice");
            nice.args(["-n", "10", "yt-dlp"]);
            nice
        } else {
            tokio::process::Command::new("yt-dlp")
        };
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
            command.arg("--cookies").arg(&copy.0);
        }
        command.arg(format!("https://music.youtube.com/watch?v={video_id}"));
        command
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null());
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(60), command.output()).await;
        drop(copy);
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
        let stream = Stream {
            itag,
            expires: crate::innertube::expiry(url),
            url: url.to_owned(),
            user_agent: (agent != "NA").then(|| agent.to_owned()),
        };
        Ok((stream, cookies.is_some()))
    }
}
