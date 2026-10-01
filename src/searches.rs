//! Recent searches: the last [`KEEP`] queries, newest first, in
//! `~/.cache/ytfast/searches.json`.

use std::path::Path;

pub const KEEP: usize = 20;

/// Puts `query` first, dropping an earlier copy of it (any case).
pub fn remember(list: &mut Vec<String>, query: &str) {
    list.retain(|q| !q.eq_ignore_ascii_case(query));
    list.insert(0, query.to_owned());
    list.truncate(KEEP);
}

/// The saved list; missing or damaged reads as empty.
pub async fn load(path: &Path) -> Vec<String> {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return Vec::new();
    };
    let mut list: Vec<String> = serde_json::from_slice(&bytes).unwrap_or_default();
    list.truncate(KEEP);
    list
}

/// Replaces the saved list (through a temporary file, so it's never half written).
pub async fn save(path: &Path, list: &[String]) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(list).map_err(std::io::Error::other)?;
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    tokio::fs::write(&temporary, bytes).await?;
    tokio::fs::rename(temporary, path).await
}
