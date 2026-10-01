//! Lyrics for a song, timed when anyone has them: YouTube Music's timed
//! lyrics (the Android Music client's lyrics page), then LRCLIB's synced
//! lyrics, then YouTube Music's plain lyrics, then LRCLIB's plain ones.

use serde::Deserialize;
use serde_json::Value;

use crate::innertube::Client;
use crate::model::{LyricLine, Lyrics, Target, Track};
use crate::parse;

const LRCLIB: &str = "https://lrclib.net/api";
/// LRCLIB asks clients to name themselves.
const USER_AGENT: &str = concat!(
    "ytfast/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ")"
);
/// How far LRCLIB's recorded length may be from the song's.
const DURATION_SLACK: f64 = 3.0;

/// The line playing at `position` seconds, if any has started.
pub fn current_line(lines: &[LyricLine], position: f64) -> Option<usize> {
    lines
        .partition_point(|l| l.start <= position)
        .checked_sub(1)
}

/// Lyrics for `track`. `browse_id` is its YouTube Music lyrics page when
/// already known; otherwise it is looked up. `duration` (seconds, 0 if
/// unknown) matches LRCLIB's records. `Ok(None)`: nobody has lyrics for it.
pub async fn fetch(
    client: &Client,
    track: &Track,
    browse_id: Option<String>,
    duration: f64,
) -> Result<Option<Lyrics>, String> {
    let mut failure = None;
    let browse_id = match browse_id {
        Some(id) => Some(id),
        None => {
            let target = Target::Watch {
                video_id: Some(track.video_id.clone()),
                playlist_id: None,
                params: None,
            };
            match client.next(&target).await {
                Ok(value) => parse::watch_next(&value).lyrics,
                Err(error) => {
                    failure = Some(error.to_string());
                    None
                }
            }
        }
    };
    if let Some(id) = &browse_id {
        match client.timed_lyrics(id).await {
            Ok(value) => {
                if let Some(lyrics) = youtube_timed(&value) {
                    return Ok(Some(lyrics));
                }
            }
            Err(error) => log::debug!("timed lyrics for {id}: {error}"),
        }
    }
    let lrclib = lrclib(client.http(), track, duration).await;
    if let Ok(Some(record)) = &lrclib
        && let Some(synced) = &record.synced
    {
        let lines = parse_lrc(synced);
        if !lines.is_empty() {
            return Ok(Some(Lyrics {
                text: lines
                    .iter()
                    .map(|l| l.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                source: Some("Lyrics from LRCLIB".into()),
                lines,
            }));
        }
    }
    if let Some(id) = &browse_id {
        match client.browse(id, None).await {
            Ok(value) => {
                if let Some(lyrics) = parse::lyrics(&value) {
                    return Ok(Some(lyrics));
                }
            }
            Err(error) => failure = Some(error.to_string()),
        }
    }
    match lrclib {
        Ok(Some(Record {
            plain: Some(plain), ..
        })) if !plain.trim().is_empty() => Ok(Some(Lyrics {
            text: plain.trim().to_owned(),
            source: Some("Lyrics from LRCLIB".into()),
            lines: Vec::new(),
        })),
        // Only a failure on every side is an error; otherwise there are none.
        Err(error) => match failure {
            Some(failure) => Err(failure),
            None if browse_id.is_none() => Err(error),
            None => Ok(None),
        },
        _ => Ok(None),
    }
}

/// `timedLyricsData` from the Android Music client's lyrics page.
fn youtube_timed(v: &Value) -> Option<Lyrics> {
    let data = parse::find(v, "timedLyricsData")?.as_array()?;
    let mut lines: Vec<LyricLine> = data
        .iter()
        .filter_map(|line| {
            let text = line.get("lyricLine")?.as_str()?.trim().to_owned();
            let start = parse::at(line, &["cueRange", "startTimeMilliseconds"])?;
            let ms = match start {
                Value::String(s) => s.parse::<f64>().ok()?,
                other => other.as_f64()?,
            };
            Some(LyricLine {
                start: ms / 1000.0,
                text,
            })
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    Some(Lyrics {
        text: lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        source: parse::find(v, "sourceMessage")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|s| !s.is_empty()),
        lines,
    })
}

/// LRC text (`[mm:ss.xx]line`, several stamps per line allowed, `[offset:ms]`).
pub fn parse_lrc(text: &str) -> Vec<LyricLine> {
    let mut offset = 0.0;
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(after) = rest.strip_prefix('[') {
            let Some(end) = after.find(']') else { break };
            let tag = &after[..end];
            rest = &after[end + 1..];
            if let Some(seconds) = lrc_time(tag) {
                stamps.push(seconds);
            } else if let Some(ms) = tag.strip_prefix("offset:") {
                offset = ms.trim().parse::<f64>().unwrap_or(0.0) / 1000.0;
            }
        }
        let line = rest.trim();
        for start in stamps {
            lines.push(LyricLine {
                start,
                text: line.to_owned(),
            });
        }
    }
    for line in &mut lines {
        line.start = (line.start - offset).max(0.0);
    }
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    lines
}

/// "mm:ss.xx" (or "mm:ss") in seconds.
fn lrc_time(tag: &str) -> Option<f64> {
    let (minutes, seconds) = tag.split_once(':')?;
    let minutes: f64 = minutes.trim().parse().ok()?;
    let seconds: f64 = seconds.trim().replace(':', ".").parse().ok()?;
    Some(minutes * 60.0 + seconds)
}

#[derive(Debug, Deserialize)]
struct Record {
    #[serde(rename = "syncedLyrics")]
    synced: Option<String>,
    #[serde(rename = "plainLyrics")]
    plain: Option<String>,
    duration: Option<f64>,
}

/// LRCLIB's record for the song: the exact match by title, artist, album and
/// length, else the search result with synced lyrics closest in length.
async fn lrclib(
    http: &reqwest::Client,
    track: &Track,
    duration: f64,
) -> Result<Option<Record>, String> {
    // The first credited artist: LRCLIB lists songs under the main one.
    let artist = track
        .artists
        .iter()
        .find(|r| r.target.is_some())
        .or_else(|| track.artists.first())
        .map(|r| r.text.trim().to_owned())
        .unwrap_or_default();
    if artist.is_empty() || track.title.is_empty() {
        return Ok(None);
    }
    let mut query = vec![
        ("track_name", track.title.clone()),
        ("artist_name", artist.clone()),
    ];
    if let Some(album) = &track.album {
        query.push(("album_name", album.text.clone()));
    }
    if duration > 0.0 {
        query.push(("duration", format!("{}", duration.round() as u64)));
    }
    let exact = http
        .get(format!("{LRCLIB}/get"))
        .header("User-Agent", USER_AGENT)
        .query(&query)
        .send()
        .await
        .map_err(|e| format!("Can't reach LRCLIB ({e})"))?;
    let exact = if exact.status().is_success() {
        exact.json::<Record>().await.ok()
    } else {
        None
    };
    if exact.as_ref().is_some_and(|r| r.synced.is_some()) {
        return Ok(exact);
    }
    let found: Vec<Record> = http
        .get(format!("{LRCLIB}/search"))
        .header("User-Agent", USER_AGENT)
        .query(&[
            ("track_name", track.title.as_str()),
            ("artist_name", &artist),
        ])
        .send()
        .await
        .map_err(|e| format!("Can't reach LRCLIB ({e})"))?
        .json()
        .await
        .unwrap_or_default();
    let distance = |r: &Record| match (duration > 0.0, r.duration) {
        (true, Some(d)) => (d - duration).abs(),
        (true, None) => f64::INFINITY,
        (false, _) => 0.0,
    };
    let closest = found
        .into_iter()
        .filter(|r| r.synced.is_some() && distance(r) <= DURATION_SLACK)
        .min_by(|a, b| distance(a).total_cmp(&distance(b)));
    Ok(closest.or(exact))
}
