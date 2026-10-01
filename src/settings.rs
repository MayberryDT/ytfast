//! Choices that last across launches, in `~/.config/ytfast/settings.json`.

use serde::{Deserialize, Serialize};

use crate::paths::Paths;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    /// The browser profile whose YouTube session to use ("google-chrome/Default");
    /// unset means the most recently used signed-in profile.
    #[serde(default)]
    pub browser_profile: Option<String>,
    /// Show a desktop notification when the song changes (off by default).
    #[serde(default)]
    pub notifications: bool,
}

impl Settings {
    /// Missing or damaged settings read as the defaults.
    pub fn load(paths: &Paths) -> Self {
        std::fs::read(paths.config.join("settings.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, paths: &Paths) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::paths::write_atomic(&paths.config.join("settings.json"), &bytes)
    }
}
