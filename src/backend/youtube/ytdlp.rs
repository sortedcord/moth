//! yt-dlp as a stream resolver: the fallback when RustyPipe fails, and the source of web-client
//! URLs for seeking (see [`super::YouTube::alternate_urls`]).

use std::process::Command;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;

use std::collections::BTreeMap;

use crate::backend::media::{AudioCodec, AudioStream, Location, StreamCatalog, StreamId, VideoCodec, VideoStream};
use crate::backend::selection::StreamPreferences;

/// Direct (non-HLS) streams only: the best video up to the preferred height in the preferred
/// codec order, plus the best audio in the preferred codec.
pub fn format_for(preferences: &StreamPreferences) -> String {
    let height = preferences.max_height;
    // yt-dlp reports VP9 as `vp9` for direct streams (and `vp09` only for HLS ones).
    let video: Vec<String> = preferences
        .video_codecs
        .iter()
        .map(|codec| {
            let prefix = match codec {
                VideoCodec::Vp9 => "vp9",
                VideoCodec::Av1 => "av01",
                VideoCodec::H264 => "avc1",
            };
            format!("bv[height<={height}][vcodec^={prefix}][protocol=https]")
        })
        .chain([format!("bv[height<={height}][protocol=https]"), "bv[protocol=https]".to_string()])
        .collect();
    let audio_prefix = match preferences.audio_codec {
        AudioCodec::Opus => "opus",
        AudioCodec::Aac => "mp4a",
    };
    // `+` binds tighter than `/`, so the alternatives need grouping.
    format!("({})+(ba[acodec^={audio_prefix}][protocol=https]/ba[protocol=https])", video.join("/"))
}

#[derive(Deserialize)]
struct Info {
    requested_formats: Option<Vec<Format>>,
}

#[derive(Deserialize)]
struct Format {
    format_id: String,
    url: String,
    vcodec: Option<String>,
    acodec: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<f64>,
    /// Total bitrate, kbit/s.
    tbr: Option<f64>,
    /// Exact size (yt-dlp's estimate, `filesize_approx`, is deliberately not used: the player
    /// treats the size as exact).
    filesize: Option<u64>,
    format_note: Option<String>,
    /// Headers the URL must be fetched with (some clients' URLs are refused without them).
    #[serde(default)]
    http_headers: BTreeMap<String, String>,
}

impl Format {
    fn location(&mut self) -> Location {
        Location {
            url: std::mem::take(&mut self.url),
            headers: std::mem::take(&mut self.http_headers).into_iter().collect(),
        }
    }
}

/// Resolve one video+audio pair. Blocking; run it on a blocking thread.
pub fn resolve(video_id: &str, format: &str) -> Result<StreamCatalog> {
    // yt-dlp may run alongside playback (for alternate URLs), so keep it out of the way.
    let output = Command::new("nice")
        .args(["-n", "10", "yt-dlp", "--no-warnings", "-f", format, "-j", "--", video_id])
        .output()
        .context("failed to run yt-dlp")?;
    if !output.status.success() {
        bail!("yt-dlp: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let info: Info = serde_json::from_slice(&output.stdout).context("unexpected yt-dlp output")?;
    let formats = info
        .requested_formats
        .ok_or_else(|| anyhow!("yt-dlp picked a combined stream; moth needs separate video and audio"))?;
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for format in formats {
        let has = |codec: &Option<String>| codec.as_deref().is_some_and(|codec| codec != "none");
        if has(&format.vcodec) {
            video.push(video_stream(format)?);
        } else if has(&format.acodec) {
            audio.push(audio_stream(format)?);
        }
    }
    if video.is_empty() || audio.is_empty() {
        bail!("yt-dlp didn't return both a video and an audio stream");
    }
    Ok(StreamCatalog { video, audio, resolver: "yt-dlp", resolver_detail: "yt-dlp".to_string(), has_alternates: false })
}

fn stream_id(format_id: &str) -> StreamId {
    // YouTube format ids are itags, sometimes with a suffix ("251-drc").
    let digits: String = format_id.chars().take_while(char::is_ascii_digit).collect();
    StreamId(digits.parse().unwrap_or(0))
}

fn bitrate(tbr: Option<f64>) -> Option<u32> {
    tbr.filter(|kbps| *kbps > 0.0).map(|kbps| (kbps * 1000.0) as u32)
}

fn video_stream(mut format: Format) -> Result<VideoStream> {
    let codec = format.vcodec.as_deref().unwrap_or_default();
    let codec = match codec {
        codec if codec.starts_with("vp9") || codec.starts_with("vp09") => VideoCodec::Vp9,
        codec if codec.starts_with("av01") => VideoCodec::Av1,
        codec if codec.starts_with("avc1") => VideoCodec::H264,
        other => bail!("unsupported video codec from yt-dlp: {other}"),
    };
    let (width, height) = (format.width.unwrap_or(0), format.height.unwrap_or(0));
    Ok(VideoStream {
        id: stream_id(&format.format_id),
        location: format.location(),
        quality: format.format_note.unwrap_or_else(|| format!("{}p", width.min(height))),
        width,
        height,
        fps: format.fps.unwrap_or(0.0).round() as u8,
        codec,
        bitrate: bitrate(format.tbr),
        size: format.filesize,
    })
}

fn audio_stream(mut format: Format) -> Result<AudioStream> {
    let codec = match format.acodec.as_deref().unwrap_or_default() {
        "opus" => AudioCodec::Opus,
        codec if codec.starts_with("mp4a") => AudioCodec::Aac,
        other => bail!("unsupported audio codec from yt-dlp: {other}"),
    };
    Ok(AudioStream {
        id: stream_id(&format.format_id),
        location: format.location(),
        codec,
        bitrate: bitrate(format.tbr),
        size: format.filesize,
        track: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_groups_alternatives() {
        let format = format_for(&StreamPreferences::default());
        assert!(format.starts_with("(bv[height<=720][vcodec^=vp9][protocol=https]/"));
        assert!(format.ends_with(")+(ba[acodec^=opus][protocol=https]/ba[protocol=https])"));
        assert_eq!(stream_id("251-drc"), StreamId(251));
    }
}
