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

    /// A lyrics page (`MPLY…`) as the Android Music app sees it, which has
    /// the timed lines. Sent without the session: with it YouTube answers 400.
    pub async fn timed_lyrics(&self, browse_id: &str) -> Result<Value> {
        let body = json!({
            "browseId": browse_id,
            "context": {"client": {"clientName": "ANDROID_MUSIC", "clientVersion": "7.21.50", "hl": "en", "gl": "US"}},
        });
        let response = self
            .http
            .post(format!("{ORIGIN}/youtubei/v1/browse?prettyPrint=false"))
            .header("Content-Type", "application/json")
            .header(
                "User-Agent",
                "com.google.android.apps.youtube.music/7.21.50 (Linux; U; Android 14) gzip",
            )
            .json(&body)
            .send()
            .await
            .map_err(offline)?;
        if !response.status().is_success() {
            return Err(ApiError::Http(response.status().as_u16()));
        }
        response
            .json()
            .await
            .map_err(|e| ApiError::Invalid(e.to_string()))
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

    /// Adds a play to the account's history, as the web player does when a
    /// song starts: fetch the player response and ping its tracking URL.
    pub async fn report_play(&self, video_id: &str) -> Result<()> {
        let player = self.call("player", json!({ "videoId": video_id })).await?;
        let base = crate::parse::at(
            &player,
            &["playbackTracking", "videostatsPlaybackUrl", "baseUrl"],
        )
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::Invalid("no playback tracking".into()))?;
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

/// How YouTube Music answered a playlist edit: done (with the response), or
/// refused with its own message ("This track is already in the playlist").
pub enum Edited {
    Done(Value),
    Refused(String),
}

/// Writes to the signed-in account, as music.youtube.com makes them (shapes
/// verified on 2026-10-01, docs/integration.md § Verified facts). A refused
/// write answers with an HTTP error (404 for unknown ids).
impl Client {
    /// Rates a song: `like/like`, `like/dislike` or `like/removelike`.
    pub async fn rate(&self, video_id: &str, status: crate::model::LikeStatus) -> Result<()> {
        use crate::model::LikeStatus;
        let endpoint = match status {
            LikeStatus::Like => "like/like",
            LikeStatus::Dislike => "like/dislike",
            LikeStatus::Indifferent => "like/removelike",
        };
        self.call(endpoint, json!({ "target": { "videoId": video_id } }))
            .await
            .map(drop)
    }

    /// Saves an album (by its `OLAK5uy_…` audio playlist) or a playlist to
    /// the library, or removes it.
    pub async fn save_to_library(&self, playlist_id: &str, save: bool) -> Result<()> {
        let endpoint = if save { "like/like" } else { "like/removelike" };
        self.call(endpoint, json!({ "target": { "playlistId": playlist_id } }))
            .await
            .map(drop)
    }

    pub async fn subscribe(&self, channel_id: &str, subscribe: bool) -> Result<()> {
        let endpoint = if subscribe {
            "subscription/subscribe"
        } else {
            "subscription/unsubscribe"
        };
        self.call(endpoint, json!({ "channelIds": [channel_id] }))
            .await
            .map(drop)
    }

    /// Creates a private playlist and returns its id.
    pub async fn create_playlist(
        &self,
        title: &str,
        description: &str,
        video_ids: &[String],
    ) -> Result<String> {
        let mut body =
            json!({ "title": title, "description": description, "privacyStatus": "PRIVATE" });
        if !video_ids.is_empty() {
            body["videoIds"] = json!(video_ids);
        }
        let value = self.call("playlist/create", body).await?;
        value
            .get("playlistId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ApiError::Invalid("no playlist id".into()))
    }

    /// `browse/edit_playlist` with `actions` (`ACTION_ADD_VIDEO`,
    /// `ACTION_REMOVE_VIDEO`, `ACTION_MOVE_VIDEO_BEFORE`,
    /// `ACTION_SET_PLAYLIST_NAME`, `ACTION_SET_PLAYLIST_DESCRIPTION`).
    pub async fn edit_playlist(&self, playlist_id: &str, actions: Vec<Value>) -> Result<Edited> {
        let value = self
            .call(
                "browse/edit_playlist",
                json!({ "playlistId": playlist_id, "actions": actions }),
            )
            .await?;
        if value.get("status").and_then(Value::as_str) == Some("STATUS_SUCCEEDED") {
            return Ok(Edited::Done(value));
        }
        let message = crate::parse::find(&value, "responseText")
            .or_else(|| crate::parse::find(&value, "successResponseText"))
            .map(|t| crate::parse::text(Some(t)))
            .unwrap_or_default();
        Ok(Edited::Refused(message))
    }

    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        self.call("playlist/delete", json!({ "playlistId": playlist_id }))
            .await
            .map(drop)
    }

    /// The account's rating of a song, from a fresh `next` request.
    pub async fn like_status(
        &self,
        video_id: &str,
    ) -> Result<Option<(String, crate::model::LikeStatus)>> {
        let target = Target::Watch {
            video_id: Some(video_id.to_owned()),
            playlist_id: None,
            params: None,
        };
        Ok(crate::parse::watch_next(&self.next(&target).await?).like)
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
