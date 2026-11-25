//! Source-neutral description of videos and their streams. Sources ([`super::source`]) produce
//! these; the UI, the player and IPC only ever see these types.
//!
//! Values are raw (counts, durations, dates), not display strings: formatting is the UI's job.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A video in a particular source, e.g. `youtube` / `aqz-KE-bpKQ`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VideoRef {
    /// [`super::source::VideoSource::id`] of the source that knows this video.
    pub source: &'static str,
    pub id: String,
}

impl fmt::Display for VideoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source, self.id)
    }
}

/// What a pasted link points at.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub video: VideoRef,
    /// Where to start (e.g. YouTube's `t=` parameter).
    pub start: Option<Duration>,
}

/// Everything on a video's watch page.
#[derive(Clone, Debug)]
pub struct VideoDetails {
    pub title: String,
    pub channel: Channel,
    pub views: Option<u64>,
    pub likes: Option<u64>,
    pub published: Option<Published>,
    pub description: String,
    pub related: Vec<VideoSummary>,
}

#[derive(Clone, Debug)]
pub struct Channel {
    /// The site's id for the channel, if it has channel pages ([`ChannelRef`]).
    pub id: Option<String>,
    pub name: String,
    pub avatar_url: Option<String>,
    pub subscribers: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Published {
    Date {
        year: i32,
        month: u8,
        day: u8,
    },
    /// As the source words it, when it has no date ("3 years ago", "Premiered …").
    Text(String),
}

/// A video in a list (related videos, the home feed, search results).
#[derive(Clone, Debug)]
pub struct VideoSummary {
    pub video: VideoRef,
    pub title: String,
    pub channel: Option<String>,
    /// The site's id for the channel, if it has channel pages ([`ChannelRef`]).
    pub channel_id: Option<String>,
    pub channel_avatar_url: Option<String>,
    pub views: Option<u64>,
    /// Relative age as the source words it, e.g. "3 years ago".
    pub age: Option<String>,
    pub duration: Option<Duration>,
    pub thumbnail_url: Option<String>,
    pub description: Option<String>,
    /// Short-form vertical video (YouTube Shorts and the like).
    pub is_short: bool,
}

impl VideoSummary {
    /// How to get to the channel that posted the video, if it names one. Some lists (the TV
    /// app's home feed) only give the name; the channel is then looked up from the video.
    pub fn channel_target(&self) -> Option<ChannelTarget> {
        self.channel.as_ref()?;
        Some(match &self.channel_id {
            Some(id) => ChannelTarget::Channel(ChannelRef { source: self.video.source, id: id.clone() }),
            None => ChannelTarget::OfVideo(self.video.clone()),
        })
    }
}

/// A channel to open: known, or the one that posted a video.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelTarget {
    Channel(ChannelRef),
    OfVideo(VideoRef),
}

/// A channel in a particular source, e.g. `youtube` / `UCHnyfMqiRRG1u-2MsSQLbXA`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ChannelRef {
    pub source: &'static str,
    pub id: String,
}

/// How a channel's videos are ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChannelOrder {
    #[default]
    Latest,
    Popular,
}

impl ChannelOrder {
    pub const ALL: [Self; 2] = [Self::Latest, Self::Popular];

    pub fn name(self) -> &'static str {
        match self {
            Self::Latest => "Latest",
            Self::Popular => "Popular",
        }
    }
}

/// A channel's page: who it is, and its videos (latest first).
#[derive(Clone, Debug)]
pub struct ChannelDetails {
    pub channel: ChannelRef,
    pub name: String,
    /// e.g. "@handle".
    pub handle: Option<String>,
    pub subscribers: Option<u64>,
    pub video_count: Option<u64>,
    pub avatar_url: Option<String>,
    pub banner_url: Option<String>,
    pub description: String,
    pub verified: bool,
    /// Whether the signed-in account follows the channel; `None` if nobody's signed in (or the
    /// site couldn't say).
    pub subscribed: Option<bool>,
    pub videos: ChannelVideos,
}

/// One page of a channel's videos.
#[derive(Clone, Debug)]
pub struct ChannelVideos {
    pub videos: Vec<VideoSummary>,
    /// Fetches the next page (opaque to everyone but the source).
    pub more: Option<ChannelMore>,
}

#[derive(Clone, Debug)]
pub struct ChannelMore {
    pub source: &'static str,
    pub token: String,
}

/// One page of video search results.
#[derive(Clone, Debug)]
pub struct SearchPage {
    pub videos: Vec<VideoSummary>,
    /// If the site corrected a typo in the query.
    pub corrected_query: Option<String>,
    /// Fetches the next page (opaque to everyone but the source).
    pub more: Option<SearchMore>,
}

#[derive(Clone, Debug)]
pub struct SearchMore {
    pub source: &'static str,
    pub token: String,
    pub visitor_data: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoCodec {
    #[serde(rename = "vp9")]
    Vp9,
    #[serde(rename = "av1")]
    Av1,
    #[serde(rename = "h264")]
    H264,
}

impl VideoCodec {
    pub const ALL: [Self; 3] = [Self::Vp9, Self::Av1, Self::H264];

    pub fn name(self) -> &'static str {
        match self {
            Self::Vp9 => "VP9",
            Self::Av1 => "AV1",
            Self::H264 => "H.264",
        }
    }
}

impl fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioCodec {
    #[serde(rename = "opus")]
    Opus,
    #[serde(rename = "aac")]
    Aac,
}

impl AudioCodec {
    pub const ALL: [Self; 2] = [Self::Opus, Self::Aac];

    pub fn name(self) -> &'static str {
        match self {
            Self::Opus => "Opus",
            Self::Aac => "AAC",
        }
    }
}

impl fmt::Display for AudioCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where to fetch a stream: the URL plus any HTTP headers the server insists on (yt-dlp's
/// URLs from some YouTube clients are refused without the matching User-Agent).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Location {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

impl Location {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into(), headers: Vec::new() }
    }
}

/// Identifies a stream within its catalog: unique among the catalog's video streams, and among
/// its audio streams. What the number means is up to the source (YouTube uses format itags), so
/// only compare ids from the same catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(pub u64);

#[derive(Clone, Debug)]
pub struct VideoStream {
    pub id: StreamId,
    pub location: Location,
    /// The source's own quality label, e.g. "1080p60" (YouTube also says "1080p" for 2048x858).
    pub quality: String,
    pub width: u32,
    pub height: u32,
    pub fps: u8,
    pub codec: VideoCodec,
    /// Average bitrate, bits per second.
    pub bitrate: Option<u32>,
    pub size: Option<u64>,
    /// A subscriber-only higher-bitrate version of a resolution (YouTube Premium's "Enhanced bitrate").
    pub premium: bool,
}

impl VideoStream {
    /// Resolution class as the source names it ("1080" for 1920x1080, 2048x858 and 1080x1920).
    pub fn resolution(&self) -> u32 {
        let digits: String = self.quality.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().unwrap_or_else(|_| self.width.min(self.height))
    }

    /// e.g. "1080p60", "720p", "1080p60 Premium".
    pub fn label(&self) -> String {
        let label = quality_label(self.resolution(), self.fps);
        if self.premium { format!("{label} Premium") } else { label }
    }

    /// The codec, and whether this is the Premium version of it, e.g. "AV1 Premium".
    pub fn format_label(&self) -> String {
        if self.premium { format!("{} Premium", self.codec) } else { self.codec.to_string() }
    }
}

/// e.g. "1080p60" (frame rates above 30 are spelled out), "720p".
pub fn quality_label(resolution: u32, fps: u8) -> String {
    if fps > 30 { format!("{resolution}p{fps}") } else { format!("{resolution}p") }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrack {
    pub id: String,
    /// Language name, e.g. "English (original)".
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Debug)]
pub struct AudioStream {
    pub id: StreamId,
    pub location: Location,
    pub codec: AudioCodec,
    /// Average bitrate, bits per second.
    pub bitrate: Option<u32>,
    pub size: Option<u64>,
    /// For videos with several audio tracks (dubs).
    pub track: Option<AudioTrack>,
}

impl AudioStream {
    /// e.g. "Opus · 135 kbps".
    pub fn label(&self) -> String {
        match self.bitrate {
            Some(bitrate) => format!("{} · {} kbps", self.codec, (bitrate + 500) / 1000),
            None => self.codec.to_string(),
        }
    }
}

/// Every playable stream of one video.
#[derive(Clone, Debug)]
pub struct StreamCatalog {
    pub video: Vec<VideoStream>,
    pub audio: Vec<AudioStream>,
    /// What produced the catalog, e.g. `rustypipe`; stable, reported over IPC.
    pub resolver: &'static str,
    /// Human-readable details, e.g. "RustyPipe (ios client)".
    pub resolver_detail: String,
    /// The source can supply alternate URLs for these streams (see
    /// [`super::source::VideoSource::alternate_urls`]).
    pub has_alternates: bool,
}

/// Progress through a video, reported to its site (e.g. for a signed-in account's history).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Watching {
    /// Playback began at this position.
    Started(Duration),
    /// Still watching, now here.
    Progress(Duration),
    /// Watched up to the first position, then sought to the second.
    Jumped(Duration, Duration),
    /// Left the video at this position.
    Stopped(Duration),
}

/// A pair of streams to play.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    /// `None` plays the audio alone.
    pub video: Option<StreamId>,
    pub audio: StreamId,
}

/// Other URLs serving byte-identical files for a [`Selection`], used when the primary URLs stop
/// serving (see [`crate::backend::player`]).
#[derive(Clone, Debug)]
pub struct AlternateUrls {
    pub video: Location,
    pub audio: Location,
}
