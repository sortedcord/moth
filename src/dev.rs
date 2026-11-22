//! Developer options, read once from `MOTH_*` environment variables at startup.

use std::path::PathBuf;

/// See CONTRIBUTING.md for what each one is for.
#[derive(Clone, Debug, Default)]
pub struct DevOptions {
    /// `MOTH_OPEN=<home card index | URL | video ID>`: open a video at startup.
    pub open: Option<String>,
    /// `MOTH_CARDS=<n>`: fill the home grid with n cards (cycling the feed).
    pub cards: Option<usize>,
    /// `MOTH_FRAME_STATS=1`: show gpui's frame-time overlay from the start.
    pub frame_stats: bool,
    /// `MOTH_PLAYER_STATS=0|1`: override the "player debug panel" setting for this run.
    pub player_stats: Option<bool>,
    /// `MOTH_RESOLVER=visionos|rustypipe|yt-dlp`: where YouTube streams come from first (the
    /// later ones in that order stay fallbacks), to test the fallbacks.
    pub resolver: Option<String>,
    /// `MOTH_FORMAT=<yt-dlp format>`: override yt-dlp's stream selection (e.g. to force AV1).
    pub ytdlp_format: Option<String>,
    /// `MOTH_DEV_MEDIA=<dir>`: pre-downloaded videos for the `local:` source (default in debug
    /// builds: `target/dev-media`, filled by `scripts/fetch-dev-media.sh`). Empty or `0` disables it.
    pub media_dir: Option<PathBuf>,
}

impl DevOptions {
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        Self {
            open: var("MOTH_OPEN"),
            cards: var("MOTH_CARDS").and_then(|n| n.parse().ok()),
            frame_stats: var("MOTH_FRAME_STATS").is_some_and(|value| value != "0"),
            player_stats: var("MOTH_PLAYER_STATS").map(|value| value != "0"),
            resolver: var("MOTH_RESOLVER"),
            ytdlp_format: var("MOTH_FORMAT"),
            media_dir: match std::env::var_os("MOTH_DEV_MEDIA") {
                Some(dir) if dir.is_empty() || dir == "0" => None,
                Some(dir) => Some(dir.into()),
                None if cfg!(debug_assertions) => Some(concat!(env!("CARGO_MANIFEST_DIR"), "/target/dev-media").into()),
                None => None,
            },
        }
    }
}
