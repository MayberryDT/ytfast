//! Where ytfast keeps things.

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::Digest;

#[derive(Clone, Debug)]
pub struct Paths {
    /// `~/.config/ytfast`: themes.
    pub config: PathBuf,
    /// `~/.cache/ytfast`: pages and covers (personal data, 0700).
    pub cache: PathBuf,
    /// `$XDG_RUNTIME_DIR/ytfast` (0700): the cookie file for yt-dlp, the
    /// mpv and single-instance sockets. Gone at logout.
    pub runtime: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "ytfast").context("no home directory")?;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("ytfast");
        let paths = Self {
            config: dirs.config_dir().to_path_buf(),
            cache: dirs.cache_dir().to_path_buf(),
            runtime,
        };
        for dir in [
            &paths.config,
            &paths.cache,
            &paths.cache.join("pages"),
            &paths.cache.join("covers"),
            &paths.runtime,
        ] {
            private_dir(dir)?;
        }
        Ok(paths)
    }

    pub fn cookie_file(&self) -> PathBuf {
        self.runtime.join("cookies.txt")
    }

    pub fn page_file(&self, key: &str) -> PathBuf {
        self.cache.join("pages").join(format!("{}.json", hash(key)))
    }

    pub fn cover_file(&self, uri: &str) -> PathBuf {
        self.cache.join("covers").join(hash(uri))
    }
}

fn private_dir(dir: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

pub fn hash(text: &str) -> String {
    sha2::Sha256::digest(text.as_bytes())[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Writes through a temporary file so a crash never leaves half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)
}
