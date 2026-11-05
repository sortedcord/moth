//! User settings, stored as JSON in `$XDG_CONFIG_HOME/moth/settings.json`.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

pub const RESOLUTIONS: [u32; 3] = [480, 720, 1080];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Highest video resolution to play (shorter edge, e.g. 720 for 720p).
    pub max_height: u32,
    /// Resolve web-client URLs with yt-dlp in the background so seeking far ahead works.
    pub seek_fallback: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { max_height: 720, seek_fallback: true }
    }
}

fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("moth")
        .join("settings.json")
}

impl Settings {
    /// Missing or unreadable settings fall back to defaults (and the problem is logged).
    pub fn load() -> Self {
        let path = path();
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                eprintln!("moth: ignoring invalid {}: {err}", path.display());
                Self::default()
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                eprintln!("moth: can't read {}: {err}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        }
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn path() -> PathBuf {
        path()
    }
}
