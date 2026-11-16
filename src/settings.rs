//! User settings, stored as JSON in `$XDG_CONFIG_HOME/moth/settings.json`.
//!
//! Anything moth does that's a matter of taste belongs here rather than in a constant. Missing
//! fields take their defaults, so the file only ever needs what the user changed.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::backend::media::{AudioCodec, VideoCodec};
use crate::backend::selection::StreamPreferences;

/// Choices offered for the maximum resolution.
pub const RESOLUTIONS: [u32; 5] = [360, 480, 720, 1080, 1440];
/// Playback speeds offered in the player menu (and for the default speed).
pub const SPEEDS: [f64; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];
/// Choices offered for the seek steps, in seconds.
pub const SEEK_STEPS: [u32; 5] = [5, 10, 15, 30, 60];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Highest resolution to pick automatically (shorter edge, e.g. 720 for 720p).
    pub max_height: u32,
    /// Video codecs in order of preference; unlisted ones come last.
    pub video_codecs: Vec<VideoCodec>,
    pub audio_codec: AudioCodec,
    /// Speed new videos start at.
    pub default_rate: f64,
    /// Keep the speed from one video to the next (as YouTube does), instead of resetting it.
    pub remember_rate: bool,
    /// Seconds: arrow keys, and J / L.
    pub seek_step: u32,
    pub seek_step_long: u32,
    pub hide_shorts: bool,
    pub animations: Animations,
    /// Show the player debug panel under the video.
    pub player_stats: bool,
    /// Stop fetching and decoding video while the window is out of sight (another workspace,
    /// scrolled away, covered); audio plays on.
    pub suspend_hidden_video: bool,
    /// Resolve alternate stream URLs in the background for sources that offer them, so
    /// seeking far ahead works (YouTube: web-client URLs via yt-dlp).
    pub seek_fallback: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Animations {
    /// Follow the desktop's reduce-motion setting.
    #[default]
    System,
    On,
    Off,
}

impl Default for Settings {
    fn default() -> Self {
        let streams = StreamPreferences::default();
        Self {
            max_height: streams.max_height,
            video_codecs: streams.video_codecs,
            audio_codec: streams.audio_codec,
            default_rate: 1.0,
            remember_rate: true,
            seek_step: 5,
            seek_step_long: 10,
            hide_shorts: true,
            animations: Animations::System,
            player_stats: true,
            suspend_hidden_video: true,
            seek_fallback: true,
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        crate::paths::config_dir().join("settings.json")
    }

    /// Missing or unreadable settings fall back to defaults (and the problem is logged).
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read(&path) {
            Ok(bytes) => Self::parse(&bytes).unwrap_or_else(|err| {
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

    fn parse(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        }
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn stream_preferences(&self) -> StreamPreferences {
        StreamPreferences {
            max_height: self.max_height,
            video_codecs: self.video_codecs.clone(),
            audio_codec: self.audio_codec,
        }
    }

    pub fn preferred_video_codec(&self) -> VideoCodec {
        self.video_codecs.first().copied().unwrap_or(VideoCodec::Vp9)
    }

    /// Make `codec` the most preferred, keeping the order of the rest.
    pub fn prefer_video_codec(&mut self, codec: VideoCodec) {
        let mut order: Vec<_> = VideoCodec::ALL.into_iter().filter(|other| *other != codec).collect();
        order.sort_by_key(|other| self.video_codecs.iter().position(|known| known == other).unwrap_or(usize::MAX));
        order.insert(0, codec);
        self.video_codecs = order;
    }

    pub fn animations_enabled(&self, system_reduce_motion: bool) -> bool {
        match self.animations {
            Animations::System => !system_reduce_motion,
            Animations::On => true,
            Animations::Off => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_files_still_load() {
        let settings = Settings::parse(br#"{"max_height":1080,"seek_fallback":false}"#).unwrap();
        assert_eq!(settings.max_height, 1080);
        assert!(!settings.seek_fallback);
        assert_eq!(settings.default_rate, Settings::default().default_rate);
    }

    #[test]
    fn preferring_a_codec_keeps_the_rest_in_order() {
        let mut settings = Settings::default();
        settings.prefer_video_codec(VideoCodec::H264);
        assert_eq!(settings.video_codecs, [VideoCodec::H264, VideoCodec::Vp9, VideoCodec::Av1]);
        settings.prefer_video_codec(VideoCodec::Av1);
        assert_eq!(settings.video_codecs, [VideoCodec::Av1, VideoCodec::H264, VideoCodec::Vp9]);
    }
}
