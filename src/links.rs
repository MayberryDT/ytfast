//! YouTube Music and YouTube links: what they open in ytfast.
//!
//! `music.youtube.com`, `www.youtube.com`, `youtube.com`, `m.youtube.com` and
//! `youtu.be` links to songs, playlists, albums, artists and searches become
//! [`Target`]s: a song (`watch?v=`, `youtu.be/<id>`) starts playing, in its
//! playlist when the link names one; everything else opens its page.

use crate::model::Target;

/// What a link opens, or `None` for anything that isn't a YouTube Music or
/// YouTube link ytfast understands. Surrounding whitespace and `<…>` are
/// ignored; the scheme may be left out.
pub fn target_from_link(link: &str) -> Option<Target> {
    let link = link.trim().trim_start_matches('<').trim_end_matches('>');
    let rest = link
        .strip_prefix("https://")
        .or_else(|| link.strip_prefix("http://"))
        .unwrap_or(link);
    let (host, path_and_query) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let path_and_query = path_and_query.split('#').next().unwrap_or("");
    let (path, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    let path = path.trim_end_matches('/');
    let param = |name: &str| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then(|| decode(value))
        })
    };
    let id = |value: Option<String>| value.filter(|v| is_id(v));

    if host == "youtu.be" {
        let video_id = Some(path.to_owned()).filter(|v| is_video_id(v))?;
        return Some(Target::Watch {
            video_id: Some(video_id),
            playlist_id: id(param("list")),
            params: None,
        });
    }
    if !matches!(
        host.as_str(),
        "music.youtube.com" | "www.youtube.com" | "youtube.com" | "m.youtube.com"
    ) {
        return None;
    }
    let mut segments = path.split('/');
    match (segments.next()?, segments.next(), segments.next()) {
        ("watch", None, _) => {
            let video_id = param("v").filter(|v| is_video_id(v));
            let playlist_id = id(param("list"));
            if video_id.is_none() && playlist_id.is_none() {
                return None;
            }
            Some(Target::Watch {
                video_id,
                playlist_id,
                params: None,
            })
        }
        ("playlist", None, _) => Some(Target::browse(format!("VL{}", id(param("list"))?))),
        ("browse" | "channel", Some(browse_id), None) if is_id(browse_id) => {
            Some(Target::browse(browse_id))
        }
        ("search" | "results", None, _) => {
            let query = param("q").or_else(|| param("search_query"))?;
            let query = query.trim().to_owned();
            (!query.is_empty()).then_some(Target::Search {
                query,
                params: None,
            })
        }
        _ => None,
    }
}

/// The first link in `text` that [`target_from_link`] understands: a dropped
/// file's contents, a `.url` file's `URL=` line, a pasted sentence.
pub fn find_link(text: &str) -> Option<&str> {
    text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .map(|token| token.strip_prefix("URL=").unwrap_or(token))
        .find(|token| token.contains("youtu") && target_from_link(token).is_some())
}

fn is_video_id(text: &str) -> bool {
    text.len() == 11 && is_id(text)
}

fn is_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Percent-decoding for query values (`+` is a space).
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(high), Some(low)) => {
                    out.push((high << 4) | low);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(digit: u8) -> Option<u8> {
    char::from(digit).to_digit(16).map(|d| d as u8)
}
