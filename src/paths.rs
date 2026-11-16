//! XDG base directories.

use std::path::PathBuf;

/// `$XDG_CONFIG_HOME/moth` (default `~/.config/moth`).
pub fn config_dir() -> PathBuf {
    base("XDG_CONFIG_HOME", ".config").join("moth")
}

/// `$XDG_CACHE_HOME/moth` (default `~/.cache/moth`).
pub fn cache_dir() -> PathBuf {
    base("XDG_CACHE_HOME", ".cache").join("moth")
}

/// `$XDG_RUNTIME_DIR`, or the temp dir (per-user file names are then up to the caller).
pub fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
}

fn base(variable: &str, home_relative: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(home_relative)))
        .unwrap_or_else(std::env::temp_dir)
}
