//! YouTube Music's InnerTube API over HTTPS.
//!
//! Signed-in requests carry the browser's cookies and a SAPISIDHASH
//! authorization, as music.youtube.com itself does. Without a session the
//! same requests browse the public catalogue.

use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha1::Digest;

use crate::auth::Session;
use crate::model::Target;

const ORIGIN: &str = "https://music.youtube.com";
const CLIENT_VERSION: &str = "1.20260923.01.00";
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// Why a request failed, in terms the interface can act on.
#[derive(Debug)]
pub enum ApiError {
    /// No connection to YouTube.
    Offline(String),
    /// YouTube rejected the session.
    Auth,
    Http(u16),
    Invalid(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Offline(detail) => write!(f, "Can't reach YouTube Music ({detail})"),
            ApiError::Auth => f.write_str("YouTube Music didn't accept the session"),
            ApiError::Http(code) => write!(f, "YouTube Music answered with an error (HTTP {code})"),
            ApiError::Invalid(detail) => {
                write!(f, "Unexpected answer from YouTube Music ({detail})")
            }
        }
    }
}

impl std::error::Error for ApiError {}

pub type Result<T, E = ApiError> = std::result::Result<T, E>;

/// A direct audio stream.
#[derive(Clone, Debug)]
pub struct Stream {
    pub itag: u32,
    pub url: String,
    pub user_agent: Option<String>,
    /// Unix seconds after which the URL stops working.
    pub expires: u64,
}

pub struct Client {
    http: reqwest::Client,
    session: RwLock<Option<Session>>,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(8))
            .gzip(true)
            .brotli(true)
            .build()
            .expect("the HTTP client builds");
        Self {
            http,
            session: RwLock::new(None),
        }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn set_session(&self, session: Option<Session>) {
        *self.session.write().expect("session lock") = session;
    }

    pub fn signed_in(&self) -> bool {
        self.session.read().expect("session lock").is_some()
    }

    fn auth_headers(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let session = self.session.read().expect("session lock");
        let Some(session) = session.as_ref() else {
            return request;
        };
        let mut request = request
            .header("Cookie", session.header())
            .header("X-Goog-AuthUser", "0");
        if let Some(sapisid) = session.sapisid() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let hash = sha1::Sha1::digest(format!("{now} {sapisid} {ORIGIN}").as_bytes());
            let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
            request = request.header("Authorization", format!("SAPISIDHASH {now}_{hex}"));
        }
        request
    }

    async fn call(&self, endpoint: &str, body: Value) -> Result<Value> {
        let mut body = body;
        body["context"] = json!({"client": {"clientName": "WEB_REMIX", "clientVersion": CLIENT_VERSION, "hl": "en", "gl": "US"}});
        let request = self
            .http
            .post(format!("{ORIGIN}/youtubei/v1/{endpoint}?prettyPrint=false"))
            .header("Content-Type", "application/json")
            .header("Origin", ORIGIN)
            .header("X-Origin", ORIGIN)
            .header("Referer", format!("{ORIGIN}/"))
            .header("User-Agent", USER_AGENT)
            .json(&body);
        let response = self.auth_headers(request).send().await.map_err(offline)?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ApiError::Auth);
        }
        if !status.is_success() {
            return Err(ApiError::Http(status.as_u16()));
        }
        let value: Value = response
            .json()
            .await
            .map_err(|e| ApiError::Invalid(e.to_string()))?;
        if self.signed_in() && crate::parse::logged_in(&value) == Some(false) {
            return Err(ApiError::Auth);
        }
        Ok(value)
    }

    pub async fn browse(&self, id: &str, params: Option<&str>) -> Result<Value> {
        let mut body = json!({ "browseId": id });
        if let Some(p) = params {
            body["params"] = json!(p);
        }
        self.call("browse", body).await
    }

    pub async fn continuation(&self, token: &str) -> Result<Value> {
        self.call("browse", json!({ "continuation": token })).await
    }

    pub async fn search(&self, query: &str, params: Option<&str>) -> Result<Value> {
        let mut body = json!({ "query": query });
        if let Some(p) = params {
            body["params"] = json!(p);
        }
        self.call("search", body).await
    }

    pub async fn search_continuation(&self, token: &str) -> Result<Value> {
        self.call("search", json!({ "continuation": token })).await
    }

    pub async fn suggestions(&self, input: &str) -> Result<Value> {
        self.call("music/get_search_suggestions", json!({ "input": input }))
            .await
    }

    /// The watch-next panel for a song, playlist, album or radio.
    pub async fn next(&self, target: &Target) -> Result<Value> {
        let Target::Watch {
            video_id,
            playlist_id,
            params,
        } = target
        else {
            return Err(ApiError::Invalid("not a playback target".into()));
        };
        let mut body = json!({ "isAudioOnly": true, "enablePersistentPlaylistPanel": true, "tunerSettingValue": "AUTOMIX_SETTING_NORMAL" });
        if let Some(v) = video_id {
            body["videoId"] = json!(v);
        }
        if let Some(p) = playlist_id {
            body["playlistId"] = json!(p);
        }
        if let Some(p) = params {
            body["params"] = json!(p);
        }
        self.call("next", body).await
    }

    pub async fn next_continuation(&self, token: &str) -> Result<Value> {
        self.call("next", json!({ "continuation": token, "isAudioOnly": true, "enablePersistentPlaylistPanel": true })).await
    }

    pub async fn account(&self) -> Result<Value> {
        self.call("account/account_menu", json!({})).await
    }

    /// The `WEB_REMIX` player response for a song: loudness data
    /// (`playerConfig.audioConfig`) and the play-tracking URL.
    pub async fn player(&self, video_id: &str) -> Result<Value> {
        self.call("player", json!({ "videoId": video_id })).await
    }

    /// Adds a play to the account's history, as the web player does when a
    /// song starts: fetch the player response and ping its tracking URL.
    pub async fn report_play(&self, video_id: &str) -> Result<()> {
        let player = self.player(video_id).await?;
        let base = crate::parse::at(
            &player,
            &["playbackTracking", "videostatsPlaybackUrl", "baseUrl"],
        )
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::Invalid("no playback tracking".into()))?;
        self.ping_playback(base).await
    }

    /// Pings a player response's `playbackTracking.videostatsPlaybackUrl.baseUrl`.
    pub async fn ping_playback(&self, base: &str) -> Result<()> {
        let cpn: String = (0..16)
            .map(|_| {
                const ALPHABET: &[u8] =
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
                ALPHABET[fastrand::usize(..ALPHABET.len())] as char
            })
            .collect();
        let request = self
            .http
            .get(format!("{base}&ver=2&c=WEB_REMIX&cpn={cpn}"))
            .header("Origin", ORIGIN)
            .header("Referer", format!("{ORIGIN}/"))
            .header("User-Agent", USER_AGENT);
        let response = self.auth_headers(request).send().await.map_err(offline)?;
        if !response.status().is_success() {
            return Err(ApiError::Http(response.status().as_u16()));
        }
        Ok(())
    }

    /// Whether YouTube Music answers at all (to tell a broken song from a lost connection).
    pub async fn reachable(&self) -> bool {
        #[cfg(feature = "e2e")]
        if crate::e2e::offline() {
            return false;
        }
        self.http
            .head(ORIGIN)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .is_ok()
    }
}

fn offline(error: reqwest::Error) -> ApiError {
    ApiError::Offline(if error.is_timeout() {
        "timed out".into()
    } else if error.is_connect() {
        "no connection".into()
    } else {
        "request failed".into()
    })
}

/// The `expire` query parameter of a googlevideo URL.
pub fn expiry(url: &str) -> u64 {
    url.split(['?', '&'])
        .find_map(|p| p.strip_prefix("expire="))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                + 3600
        })
}
