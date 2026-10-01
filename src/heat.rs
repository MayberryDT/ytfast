//! YouTube's "most replayed" heat for a song: how often each part of it is
//! replayed, as the seek bar's ridge, and the part people come back to.
//!
//! The `WEB` client's `next` on www.youtube.com answers anonymously (~1 s)
//! with `frameworkUpdates…macroMarkersListEntity.markersList` holding 100
//! `MARKER_TYPE_HEATMAP` markers (`startMillis`, `durationMillis`,
//! `intensityScoreNormalized`). Sent without the session: it is public data.

use serde_json::{Value, json};

const ORIGIN: &str = "https://www.youtube.com";
const WEB_VERSION: &str = "2.20250930.01.00";
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// The start of a song always reads as heavily replayed (every play starts
/// there), so the peak is looked for after this fraction of it.
const SKIP_START: f64 = 0.05;
/// The most replayed part runs on either side of its peak while the heat
/// stays above this fraction of the peak's.
const PART: f32 = 0.85;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marker {
    /// Seconds into the song.
    pub start: f64,
    pub duration: f64,
    /// 0 (rarely replayed) to 1 (the most).
    pub intensity: f32,
}

/// The most replayed part, in seconds into the song.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Peak {
    pub start: f64,
    pub end: f64,
    /// The middle of the hottest marker.
    pub at: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Heat {
    /// Earliest first, covering the whole song.
    pub markers: Vec<Marker>,
    pub peak: Option<Peak>,
}

impl Heat {
    /// How long the markers say the song is.
    pub fn length(&self) -> f64 {
        self.markers.last().map_or(0.0, |m| m.start + m.duration)
    }

    fn from_markers(markers: Vec<Marker>) -> Option<Self> {
        if markers.len() < 8 {
            return None;
        }
        let mut heat = Self {
            markers,
            peak: None,
        };
        heat.peak = peak(&heat.markers, heat.length());
        Some(heat)
    }
}

/// The hottest marker after the first 5 %, widened to the run of markers
/// around it that are nearly as hot.
fn peak(markers: &[Marker], length: f64) -> Option<Peak> {
    let from = length * SKIP_START;
    let (top, hottest) = markers
        .iter()
        .enumerate()
        .filter(|(_, m)| m.start >= from)
        .max_by(|a, b| a.1.intensity.total_cmp(&b.1.intensity))?;
    if hottest.intensity <= 0.0 {
        return None;
    }
    let floor = hottest.intensity * PART;
    let mut first = top;
    while first > 0 && markers[first - 1].intensity >= floor && markers[first - 1].start >= from {
        first -= 1;
    }
    let mut last = top;
    while last + 1 < markers.len() && markers[last + 1].intensity >= floor {
        last += 1;
    }
    Some(Peak {
        start: markers[first].start,
        end: markers[last].start + markers[last].duration,
        at: hottest.start + hottest.duration / 2.0,
    })
}

/// The heat markers in a `next` response, if it has any.
pub fn parse(response: &Value) -> Option<Heat> {
    let mutations = crate::parse::at(
        response,
        &["frameworkUpdates", "entityBatchUpdate", "mutations"],
    )?
    .as_array()?;
    let millis = |v: Option<&Value>| -> Option<f64> {
        let v = v?;
        let ms = match v.as_str() {
            Some(s) => s.parse::<f64>().ok()?,
            None => v.as_f64()?,
        };
        Some(ms / 1000.0)
    };
    mutations.iter().find_map(|m| {
        let list = crate::parse::at(m, &["payload", "macroMarkersListEntity", "markersList"])?;
        if list.get("markerType")?.as_str()? != "MARKER_TYPE_HEATMAP" {
            return None;
        }
        let mut markers: Vec<Marker> = list
            .get("markers")?
            .as_array()?
            .iter()
            .filter_map(|m| {
                Some(Marker {
                    start: millis(m.get("startMillis"))?,
                    duration: millis(m.get("durationMillis"))?,
                    intensity: m.get("intensityScoreNormalized")?.as_f64()?.clamp(0.0, 1.0) as f32,
                })
            })
            .collect();
        markers.sort_by(|a, b| a.start.total_cmp(&b.start));
        Heat::from_markers(markers)
    })
}

/// Asks YouTube for a song's heat, without the session. `Ok(None)`: the
/// song has none (too few plays, or not a video YouTube keeps heat for).
pub async fn fetch(http: &reqwest::Client, video_id: &str) -> Result<Option<Heat>, String> {
    let body = json!({
        "videoId": video_id,
        "context": {"client": {"clientName": "WEB", "clientVersion": WEB_VERSION, "hl": "en", "gl": "US"}},
    });
    let response = http
        .post(format!("{ORIGIN}/youtubei/v1/next?prettyPrint=false"))
        .header("Content-Type", "application/json")
        .header("Origin", ORIGIN)
        .header("Referer", format!("{ORIGIN}/"))
        .header("User-Agent", USER_AGENT)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let value: Value = response.json().await.map_err(|e| e.to_string())?;
    Ok(parse(&value))
}
