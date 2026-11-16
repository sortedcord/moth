//! Developer options, read once from `MOTH_*` environment variables at startup.

/// See CONTRIBUTING.md for what each one is for.
#[derive(Clone, Debug, Default)]
pub struct DevOptions {
    /// `MOTH_OPEN=<home card index | URL | video ID>`: open a video at startup.
    pub open: Option<String>,
    /// `MOTH_CARDS=<n>`: fill the home grid with n cards (cycling the samples).
    pub cards: Option<usize>,
    /// `MOTH_FRAME_STATS=1`: show gpui's frame-time overlay from the start.
    pub frame_stats: bool,
    /// `MOTH_PLAYER_STATS=0|1`: override the "player debug panel" setting for this run.
    pub player_stats: Option<bool>,
    /// `MOTH_FORMAT=<yt-dlp format>`: override yt-dlp's stream selection (e.g. to force AV1).
    pub ytdlp_format: Option<String>,
}

impl DevOptions {
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        Self {
            open: var("MOTH_OPEN"),
            cards: var("MOTH_CARDS").and_then(|n| n.parse().ok()),
            frame_stats: var("MOTH_FRAME_STATS").is_some_and(|value| value != "0"),
            player_stats: var("MOTH_PLAYER_STATS").map(|value| value != "0"),
            ytdlp_format: var("MOTH_FORMAT"),
        }
    }
}
