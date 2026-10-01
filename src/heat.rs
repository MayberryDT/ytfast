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
    /// The ridge drawn from the markers: (seconds into the song, 0..=1)
    /// along a smooth curve, earliest first. Worked out once, here, so
    /// drawing it only maps the points onto the seek line.
    pub curve: Vec<(f64, f32)>,
}

impl Heat {
    /// How long the markers say the song is.
    pub fn length(&self) -> f64 {
        self.markers.last().map_or(0.0, |m| m.start + m.duration)
    }

    /// The ridge's height (0..=1) `at` seconds into the song.
    pub fn value_at(&self, at: f64) -> f32 {
        let points = &self.curve;
        let i = points.partition_point(|p| p.0 < at);
        match (i.checked_sub(1).and_then(|j| points.get(j)), points.get(i)) {
            (Some(a), Some(b)) if b.0 > a.0 => {
                a.1 + (b.1 - a.1) * ((at - a.0) / (b.0 - a.0)) as f32
            }
            (_, Some(b)) => b.1,
            (Some(a), None) => a.1,
            (None, None) => 0.0,
        }
    }

    fn from_markers(markers: Vec<Marker>) -> Option<Self> {
        if markers.len() < 8 {
            return None;
        }
        let mut heat = Self {
            markers,
            peak: None,
            curve: Vec::new(),
        };
        heat.peak = peak(&heat.markers, heat.length());
        heat.curve = curve(&heat.markers, heat.length());
        Some(heat)
    }
}

/// The heat, lightly smoothed, as a Catmull-Rom curve through the markers'
/// middles from the start of the song to its end.
fn curve(m: &[Marker], length: f64) -> Vec<(f64, f32)> {
    let smooth = |i: usize| {
        let at = |j: isize| m[j.clamp(0, m.len() as isize - 1) as usize].intensity;
        let i = i as isize;
        (at(i - 1) + 2.0 * at(i) + at(i + 1)) / 4.0
    };
    let mut points = Vec::with_capacity(m.len() + 2);
    points.push((0.0, smooth(0)));
    for (i, marker) in m.iter().enumerate() {
        points.push((marker.start + marker.duration / 2.0, smooth(i)));
    }
    points.push((length, smooth(m.len() - 1)));
    let n = points.len();
    let get = |i: isize| points[i.clamp(0, n as isize - 1) as usize];
    let mut out = Vec::with_capacity(n * 3);
    for i in 0..n as isize - 1 {
        let (p0, p1, p2, p3) = (get(i - 1), get(i), get(i + 1), get(i + 2));
        for k in 0..3 {
            let t = k as f64 / 3.0;
            let cr = |a: f64, b: f64, c: f64, d: f64| {
                0.5 * (2.0 * b
                    + (c - a) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t
                    + (3.0 * b - a - 3.0 * c + d) * t * t * t)
            };
            let value = cr(
                f64::from(p0.1),
                f64::from(p1.1),
                f64::from(p2.1),
                f64::from(p3.1),
            );
            out.push((cr(p0.0, p1.0, p2.0, p3.0), value.clamp(0.0, 1.0) as f32));
        }
    }
    out.push(points[n - 1]);
    out
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
