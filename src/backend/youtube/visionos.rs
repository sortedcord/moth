//! Streams from YouTube's visionOS client, asked directly over InnerTube.
//!
//! Its stream URLs are plain: no signature to decipher, no `n` throttling parameter, no PO token.
//! Unlike the iOS client's URLs, which RustyPipe gets and which refuse anything much further than
//! a little ahead of real-time playback, they serve any byte range from the start. So a far seek
//! is one range request, as in a browser, instead of a 403 and a switch to yt-dlp's URLs.
//!
//! The request needs a visitor ID (`visitorData`); without one YouTube asks to sign in. RustyPipe
//! keeps a cache of them, and a refused one is dropped and replaced once.
//!
//! Signed out, this client also still has a home feed (popular videos for the region YouTube
//! places the visitor in), where the web client's is empty until there's watch history.

use anyhow::{Context as _, Result, bail};
use rustypipe::client::RustyPipe;
use serde::Deserialize;

use crate::backend::media::{
    AudioCodec, AudioStream, AudioTrack, Location, StreamCatalog, StreamId, VideoCodec, VideoStream, VideoSummary,
};

const API_URL: &str = "https://www.youtube.com/youtubei/v1/";
const CLIENT_NAME: &str = "VISIONOS";
/// `X-YouTube-Client-Name` for [`CLIENT_NAME`].
const CLIENT_ID: &str = "101";
const CLIENT_VERSION: &str = "1.02";
const OS_VERSION: &str = "26.5.23O471";
const DEVICE_MODEL: &str = "RealityDevice17,1";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) \
                          Version/26.0 Safari/605.1.15";

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// YouTube won't play the video to this client (signed out): it's age-restricted, members-only,
/// private, wants proof the viewer isn't a bot, or doesn't exist.
#[derive(Debug)]
pub struct Refused {
    pub status: String,
    pub reason: Option<String>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status, self.reason.as_deref().unwrap_or("no reason given"))
    }
}

impl std::error::Error for Refused {}

/// What the visionOS client says about a video.
pub enum Outcome {
    Streams(StreamCatalog),
    /// A live stream: there are no files to fetch ranges of.
    Live,
}

/// Ask for a video's streams. An error means falling back to another resolver is worth a try.
pub async fn resolve(http: &reqwest::Client, rustypipe: &RustyPipe, id: &str) -> Result<Outcome> {
    let query = rustypipe.query();
    let visitor_data = query.get_visitor_data(false).await?;
    let response = match player(http, id, &visitor_data).await? {
        response if response.wants_sign_in() => {
            // This visitor ID is no good (any more); try once with a new one.
            query.remove_visitor_data(&visitor_data);
            player(http, id, &query.get_visitor_data(true).await?).await?
        }
        response => response,
    };
    response.outcome()
}

async fn player(http: &reqwest::Client, id: &str, visitor_data: &str) -> Result<Response> {
    let body = serde_json::json!({"videoId": id, "contentCheckOk": true, "racyCheckOk": true});
    let bytes = request(http, "player", body, visitor_data).await?;
    serde_json::from_slice(&bytes).context("unexpected player response")
}

/// The home feed's first page: the videos of every shelf on it, without duplicates.
pub async fn home(http: &reqwest::Client, rustypipe: &RustyPipe) -> Result<Vec<VideoSummary>> {
    let visitor_data = rustypipe.query().get_visitor_data(false).await?;
    let body = serde_json::json!({"browseId": "FEwhat_to_watch"});
    let bytes = request(http, "browse", body, &visitor_data).await?;
    let response: serde_json::Value = serde_json::from_slice(&bytes).context("unexpected home feed response")?;
    let videos = super::feed::videos(&response);
    if videos.is_empty() {
        bail!("the home feed is empty");
    }
    Ok(videos)
}

/// POST `body` (plus the client context) to an InnerTube endpoint; returns the response body.
async fn request(
    http: &reqwest::Client,
    endpoint: &str,
    mut body: serde_json::Value,
    visitor_data: &str,
) -> Result<Vec<u8>> {
    body["context"] = serde_json::json!({"client": {
            "clientName": CLIENT_NAME,
            "clientVersion": CLIENT_VERSION,
            "osName": "visionOS",
            "osVersion": OS_VERSION,
            "platform": "MOBILE",
            "deviceMake": "Apple",
            "deviceModel": DEVICE_MODEL,
            "userAgent": USER_AGENT,
            "visitorData": visitor_data,
            "hl": "en",
    }});
    let response = http
        .post(format!("{API_URL}{endpoint}?prettyPrint=false"))
        .header("Content-Type", "application/json")
        .header("User-Agent", USER_AGENT)
        .header("X-YouTube-Client-Name", CLIENT_ID)
        .header("X-YouTube-Client-Version", CLIENT_VERSION)
        .header("X-Goog-Visitor-Id", visitor_data)
        .header("Origin", "https://www.youtube.com")
        .timeout(TIMEOUT)
        .body(body.to_string())
        .send()
        .await
        .with_context(|| format!("{endpoint} request failed"))?;
    let status = response.status();
    let bytes = response.bytes().await.with_context(|| format!("{endpoint} request failed"))?;
    if !status.is_success() {
        bail!("{endpoint} request: HTTP {status}");
    }
    Ok(bytes.to_vec())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    playability_status: Playability,
    streaming_data: Option<StreamingData>,
    video_details: Option<Details>,
}

#[derive(Deserialize)]
struct Playability {
    status: String,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Details {
    #[serde(default)]
    is_live: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamingData {
    #[serde(default)]
    adaptive_formats: Vec<Format>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Format {
    itag: u64,
    url: Option<String>,
    /// e.g. `video/webm; codecs="vp9"`.
    mime_type: String,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<u8>,
    /// e.g. "1080p60" (video only).
    quality_label: Option<String>,
    average_bitrate: Option<u32>,
    bitrate: Option<u32>,
    /// Exact size in bytes, as a decimal string.
    content_length: Option<String>,
    audio_track: Option<Track>,
    /// A "stable volume" re-encode, listed under the same itag as the regular file.
    #[serde(default)]
    is_drc: bool,
    drm_families: Option<serde_json::Value>,
    color_info: Option<ColorInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Track {
    id: String,
    display_name: String,
    #[serde(default)]
    audio_is_default: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ColorInfo {
    transfer_characteristics: Option<String>,
}

impl Response {
    fn wants_sign_in(&self) -> bool {
        self.playability_status.status == "LOGIN_REQUIRED"
    }

    fn outcome(self) -> Result<Outcome> {
        if self.video_details.as_ref().is_some_and(|details| details.is_live) {
            return Ok(Outcome::Live);
        }
        let status = self.playability_status;
        if status.status != "OK" {
            return Err(Refused { status: status.status, reason: status.reason }.into());
        }
        let formats = self.streaming_data.map(|data| data.adaptive_formats).unwrap_or_default();
        let mut video = Vec::new();
        let mut audio = Vec::new();
        for format in formats {
            if format.drm_families.is_some() || format.is_drc {
                continue;
            }
            if format.mime_type.starts_with("video/") {
                video.extend(format.video_stream());
            } else if format.mime_type.starts_with("audio/") {
                audio.extend(format.audio_stream());
            }
        }
        if video.is_empty() || audio.is_empty() {
            bail!("no playable streams");
        }
        Ok(Outcome::Streams(StreamCatalog {
            video,
            audio,
            resolver: "visionos",
            resolver_detail: "YouTube (visionOS client)".to_string(),
            has_alternates: false,
        }))
    }
}

impl Format {
    /// The codec named in the MIME type, e.g. `vp9` in `video/webm; codecs="vp9"`.
    fn codec(&self) -> &str {
        let codecs = self.mime_type.split_once("codecs=").map_or("", |(_, codecs)| codecs);
        codecs.trim_matches(|c: char| c == '"' || c.is_whitespace())
    }

    fn is_hdr(&self) -> bool {
        let transfer = self.color_info.as_ref().and_then(|info| info.transfer_characteristics.as_deref());
        matches!(transfer, Some(t) if t.ends_with("SMPTEST2084") || t.ends_with("ARIB_STD_B67"))
            || self.quality_label.as_deref().is_some_and(|label| label.contains("HDR"))
    }

    fn size(&self) -> Option<u64> {
        self.content_length.as_deref().and_then(|length| length.parse().ok())
    }

    fn video_stream(self) -> Option<VideoStream> {
        let codec = match self.codec() {
            codec if codec.starts_with("vp9") || codec.starts_with("vp09") => VideoCodec::Vp9,
            codec if codec.starts_with("av01") => VideoCodec::Av1,
            codec if codec.starts_with("avc1") => VideoCodec::H264,
            _ => return None,
        };
        if self.is_hdr() {
            return None;
        }
        let size = self.size();
        let (width, height) = (self.width.unwrap_or(0), self.height.unwrap_or(0));
        Some(VideoStream {
            id: StreamId(self.itag),
            location: Location::new(self.url?),
            quality: self.quality_label.unwrap_or_else(|| format!("{}p", width.min(height))),
            width,
            height,
            fps: self.fps.unwrap_or(0),
            codec,
            bitrate: self.average_bitrate.or(self.bitrate),
            size,
            premium: false,
        })
    }

    fn audio_stream(self) -> Option<AudioStream> {
        let codec = match self.codec() {
            "opus" => AudioCodec::Opus,
            codec if codec.starts_with("mp4a") => AudioCodec::Aac,
            _ => return None,
        };
        let size = self.size();
        Some(AudioStream {
            id: StreamId(self.itag),
            location: Location::new(self.url?),
            codec,
            bitrate: self.average_bitrate.or(self.bitrate),
            size,
            track: self.audio_track.map(|track| AudioTrack {
                id: track.id,
                name: track.display_name,
                is_default: track.audio_is_default,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Outcome> {
        serde_json::from_str::<Response>(json).expect("parses").outcome()
    }

    #[test]
    fn reads_streams_and_skips_drc_and_hdr() {
        let json = r#"{"playabilityStatus": {"status": "OK"}, "videoDetails": {"videoId": "x"},
            "streamingData": {"adaptiveFormats": [
                {"itag": 248, "url": "https://v/248", "mimeType": "video/webm; codecs=\"vp9\"", "width": 1920,
                 "height": 804, "fps": 24, "qualityLabel": "1080p", "averageBitrate": 943000, "contentLength": "54681167"},
                {"itag": 337, "url": "https://v/337", "mimeType": "video/webm; codecs=\"vp09.02.51.10.01.09.16.09.00\"",
                 "width": 3840, "height": 2160, "fps": 60, "qualityLabel": "2160p60 HDR",
                 "colorInfo": {"transferCharacteristics": "COLOR_TRANSFER_CHARACTERISTICS_SMPTEST2084"}},
                {"itag": 399, "url": "https://v/399", "mimeType": "video/mp4; codecs=\"av01.0.08M.08\"", "width": 1920,
                 "height": 804, "fps": 24, "qualityLabel": "1080p"},
                {"itag": 251, "url": "https://a/251", "mimeType": "audio/webm; codecs=\"opus\"", "averageBitrate": 130000,
                 "contentLength": "7973641", "audioTrack": {"id": "en.4", "displayName": "English original", "audioIsDefault": true}},
                {"itag": 251, "url": "https://a/251-drc", "mimeType": "audio/webm; codecs=\"opus\"", "isDrc": true},
                {"itag": 140, "url": "https://a/140", "mimeType": "audio/mp4; codecs=\"mp4a.40.2\""}
            ]}}"#;
        let Outcome::Streams(catalog) = parse(json).expect("ok") else { panic!("not live") };
        let video: Vec<_> = catalog.video.iter().map(|v| (v.id.0, v.codec)).collect();
        assert_eq!(video, [(248, VideoCodec::Vp9), (399, VideoCodec::Av1)]);
        assert_eq!(catalog.video[0].size, Some(54_681_167));
        assert_eq!(catalog.video[0].label(), "1080p");
        let audio: Vec<_> = catalog.audio.iter().map(|a| (a.location.url.as_str(), a.codec)).collect();
        assert_eq!(audio, [("https://a/251", AudioCodec::Opus), ("https://a/140", AudioCodec::Aac)]);
        let track = catalog.audio[0].track.as_ref().expect("track");
        assert_eq!((track.id.as_str(), track.is_default), ("en.4", true));
        assert!(!catalog.has_alternates);
    }

    #[test]
    fn reports_live_and_refusals() {
        let live = r#"{"playabilityStatus": {"status": "OK"}, "videoDetails": {"isLive": true}}"#;
        assert!(matches!(parse(live), Ok(Outcome::Live)));
        let refused =
            r#"{"playabilityStatus": {"status": "LOGIN_REQUIRED", "reason": "Sign in to confirm you're not a bot"}}"#;
        let response: Response = serde_json::from_str(refused).expect("parses");
        assert!(response.wants_sign_in());
        let error = response.outcome().err().expect("refused").to_string();
        assert!(error.contains("not a bot"), "{error}");
    }
}
