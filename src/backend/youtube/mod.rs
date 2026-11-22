//! YouTube, via YouTube's internal InnerTube API: RustyPipe for video details, related videos
//! and the home feed; streams from the first of these that works (see [`Resolver`]):
//!
//! 1. The visionOS client ([`visionos`]), whose URLs serve any byte range from the start.
//! 2. RustyPipe, which gets streams from YouTube's iOS client. Those URLs only serve a little
//!    ahead of real-time playback, so the catalog offers web-client URLs for the same files as
//!    alternate URLs (resolved by yt-dlp), which the player switches to when a long seek gets
//!    refused.
//! 3. yt-dlp alone.
//!
//! yt-dlp's URLs have a catch of their own: YouTube may only start serving them a few seconds
//! after handing them out. They leave this module once they work (see [`resolve_with_ytdlp`]).

mod visionos;
mod ytdlp;

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use rustypipe::client::RustyPipe;
use rustypipe::model::richtext::ToPlaintext as _;
use rustypipe::model::{
    AudioCodec as RpAudioCodec, Thumbnail, UrlTarget, VideoCodec as RpVideoCodec, VideoItem, VideoPlayer,
};

use super::media::{
    AlternateUrls, AudioCodec, AudioStream, AudioTrack, Channel, Link, Location, Published, StreamCatalog, StreamId,
    VideoCodec, VideoDetails, VideoRef, VideoStream, VideoSummary,
};
use super::selection::StreamPreferences;
use super::source::{BoxFuture, VideoSource};

pub const ID: &str = "youtube";

const HOSTS: [&str; 7] = [
    "youtube.com",
    "www.youtube.com",
    "m.youtube.com",
    "music.youtube.com",
    "youtu.be",
    "youtube-nocookie.com",
    "www.youtube-nocookie.com",
];

/// Whether `url` is a link to YouTube.
pub fn claims_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    HOSTS.contains(&host)
}

/// YouTube video IDs: 11 characters of URL-safe base64.
fn is_video_id(input: &str) -> bool {
    input.len() == 11 && input.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Where stream URLs come from, in the order they're tried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Resolver {
    #[default]
    VisionOs,
    RustyPipe,
    YtDlp,
}

impl Resolver {
    /// `visionos`, `rustypipe` or `yt-dlp`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "visionos" => Some(Self::VisionOs),
            "rustypipe" => Some(Self::RustyPipe),
            "yt-dlp" | "ytdlp" => Some(Self::YtDlp),
            _ => None,
        }
    }
}

const LIVE: &str = "Live streams aren't supported yet";

pub struct YouTube {
    client: RustyPipe,
    http: reqwest::Client,
    /// Dev override: start the resolver chain further down.
    first: Resolver,
    /// Dev override for yt-dlp's format selection.
    ytdlp_format: Option<String>,
}

impl YouTube {
    /// Must run inside the tokio runtime: RustyPipe's builder spawns background work.
    pub async fn new(http: reqwest::Client, first: Resolver, ytdlp_format: Option<String>) -> Result<Self> {
        let storage_dir = crate::paths::cache_dir();
        std::fs::create_dir_all(&storage_dir)
            .map_err(|err| anyhow!("failed to create {}: {err}", storage_dir.display()))?;
        let client = RustyPipe::builder().storage_dir(storage_dir).build()?;
        Ok(Self { client, http, first, ytdlp_format })
    }
}

impl VideoSource for YouTube {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "YouTube"
    }

    fn claims(&self, input: &str) -> bool {
        claims_url(input) || is_video_id(input)
    }

    fn resolve(&self, input: String) -> BoxFuture<Link> {
        let client = self.client.clone();
        Box::pin(async move {
            match client.query().resolve_string(&input, false).await {
                Ok(UrlTarget::Video { id, start_time }) => Ok(Link {
                    video: VideoRef { source: ID, id },
                    start: (start_time > 0).then(|| Duration::from_secs(start_time.into())),
                }),
                Ok(_) => bail!("Only video links are supported for now"),
                Err(_) => bail!("That doesn't look like a YouTube video link"),
            }
        })
    }

    fn details(&self, id: String) -> BoxFuture<VideoDetails> {
        let client = self.client.clone();
        Box::pin(async move {
            let details = client.query().video_details(&id).await?;
            let channel = details.channel;
            Ok(VideoDetails {
                title: details.name,
                channel: Channel {
                    avatar_url: pick_thumbnail(&channel.avatar, 80),
                    subscribers: channel.subscriber_count,
                    name: channel.name,
                },
                views: Some(details.view_count),
                likes: details.like_count.map(u64::from),
                published: details
                    .publish_date
                    .map(|date| Published::Date { year: date.year(), month: u8::from(date.month()), day: date.day() })
                    .or(details.publish_date_txt.map(Published::Text)),
                description: details.description.to_plaintext(),
                related: details.recommended.items.into_iter().map(summary).collect(),
            })
        })
    }

    fn streams(&self, id: String, preferences: StreamPreferences) -> BoxFuture<StreamCatalog> {
        let client = self.client.clone();
        let http = self.http.clone();
        let first = self.first;
        let ytdlp_format = self.ytdlp_format.clone();
        Box::pin(async move {
            let mut failures = Vec::new();
            if first <= Resolver::VisionOs {
                match visionos::resolve(&http, &client, &id).await {
                    Ok(visionos::Outcome::Streams(mut catalog)) => {
                        number_tracks(&mut catalog);
                        return Ok(catalog);
                    }
                    Ok(visionos::Outcome::Live) => bail!(LIVE),
                    Err(err) => {
                        eprintln!("moth: the visionOS client gave no streams, trying RustyPipe: {err:#}");
                        failures.push(format!("visionOS client: {err:#}"));
                    }
                }
            }
            if first <= Resolver::RustyPipe {
                let error = match client.query().player(&id).await {
                    Ok(player) if player.details.is_live => bail!(LIVE),
                    Ok(player) => {
                        let mut catalog = catalog(&player);
                        number_tracks(&mut catalog);
                        if !catalog.video.is_empty() && !catalog.audio.is_empty() {
                            return Ok(catalog);
                        }
                        anyhow!("no playable streams")
                    }
                    Err(err) => err.into(),
                };
                eprintln!("moth: RustyPipe stream resolution failed, trying yt-dlp: {error:#}");
                failures.push(format!("RustyPipe: {error:#}"));
            }
            let format = ytdlp_format.unwrap_or_else(|| ytdlp::format_for(&preferences));
            match resolve_with_ytdlp(id, format).await {
                Ok(mut catalog) => {
                    if !failures.is_empty() {
                        catalog.resolver_detail = "yt-dlp (fallback)".to_string();
                    }
                    Ok(catalog)
                }
                Err(error) => {
                    failures.push(format!("yt-dlp: {error:#}"));
                    bail!("{}", failures.join("\n"))
                }
            }
        })
    }

    fn alternate_urls(&self, id: String, video: &VideoStream, audio: &AudioStream) -> Option<BoxFuture<AlternateUrls>> {
        let audio_format = match &audio.track {
            // Dubs share the itag; yt-dlp tells them apart by language (and lists the default
            // track's "stable volume" copy under the same itag, as `-drc`).
            Some(track) => {
                let language = track.id.split('.').next().unwrap_or_default();
                format!("(ba[format_id^={itag}-][format_id!$=-drc][language={language}]/{itag})", itag = itag(audio.id))
            }
            None => itag(audio.id).to_string(),
        };
        let format = format!("{}+{audio_format}", itag(video.id));
        Some(Box::pin(async move {
            let catalog = resolve_with_ytdlp(id, format).await?;
            match (catalog.video.into_iter().next(), catalog.audio.into_iter().next()) {
                (Some(video), Some(audio)) => Ok(AlternateUrls { video: video.location, audio: audio.location }),
                _ => bail!("yt-dlp returned no alternate URLs"),
            }
        }))
    }

    fn web_url(&self, id: &str) -> String {
        format!("https://www.youtube.com/watch?v={id}")
    }

    fn thumbnail_url(&self, id: &str) -> Option<String> {
        Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"))
    }
}

/// Give each audio track's streams their own ids: every dub of a format has the same itag, so
/// the id is the itag plus, in the bits above it, the track's position among the video's tracks.
fn number_tracks(catalog: &mut StreamCatalog) {
    let mut tracks: Vec<String> = Vec::new();
    for stream in &mut catalog.audio {
        let Some(track) = &stream.track else { continue };
        let index = tracks.iter().position(|known| *known == track.id).unwrap_or_else(|| {
            tracks.push(track.id.clone());
            tracks.len() - 1
        });
        stream.id = StreamId(stream.id.0 | (index as u64) << 32);
    }
}

/// The itag of a stream (see [`number_tracks`]).
fn itag(id: StreamId) -> u64 {
    id.0 & 0xffff_ffff
}

/// Have yt-dlp resolve `format` of a video, and return its catalog once the URLs in it work.
async fn resolve_with_ytdlp(id: String, format: String) -> Result<StreamCatalog> {
    let resolved = tokio::task::spawn_blocking(move || ytdlp::resolve(&id, &format)).await??;
    let wait = resolved.usable_in();
    if !wait.is_zero() {
        eprintln!("moth: YouTube serves yt-dlp's URLs in {:.1}s; holding them until then", wait.as_secs_f64());
        tokio::time::sleep(wait).await;
    }
    Ok(resolved.catalog)
}

fn catalog(player: &VideoPlayer) -> StreamCatalog {
    let video = player
        .video_only_streams
        .iter()
        .filter(|stream| !stream.hdr && stream.drm_track_type.is_none())
        .filter_map(|stream| {
            let codec = match stream.codec {
                RpVideoCodec::Vp9 => VideoCodec::Vp9,
                RpVideoCodec::Av01 => VideoCodec::Av1,
                RpVideoCodec::Avc1 => VideoCodec::H264,
                _ => return None,
            };
            Some(VideoStream {
                id: StreamId(stream.itag.into()),
                location: Location::new(&stream.url),
                quality: stream.quality.clone(),
                width: stream.width,
                height: stream.height,
                fps: stream.fps,
                codec,
                bitrate: Some(stream.average_bitrate),
                size: stream.size,
            })
        })
        .collect();
    // The iOS client also lists "stable volume" (DRC) re-encodes under the same itags as the
    // regular files. They must be skipped, not just deprioritized: alternate URLs are requested
    // by itag and would serve the regular file, so switching mid-stream would corrupt playback.
    let audio = player
        .audio_streams
        .iter()
        .filter(|stream| !stream.url.contains("drc%3D1") && stream.drm_track_type.is_none())
        .filter_map(|stream| {
            let codec = match stream.codec {
                RpAudioCodec::Opus => AudioCodec::Opus,
                RpAudioCodec::Mp4a => AudioCodec::Aac,
                _ => return None,
            };
            Some(AudioStream {
                id: StreamId(stream.itag.into()),
                location: Location::new(&stream.url),
                codec,
                bitrate: Some(stream.average_bitrate),
                size: Some(stream.size),
                track: stream.track.as_ref().map(|track| AudioTrack {
                    id: track.id.clone(),
                    name: track.lang_name.clone(),
                    is_default: track.is_default,
                }),
            })
        })
        .collect();
    let client = format!("{:?}", player.client_type).to_lowercase();
    StreamCatalog {
        video,
        audio,
        resolver: "rustypipe",
        resolver_detail: format!("RustyPipe ({client} client)"),
        has_alternates: true,
    }
}

fn summary(item: VideoItem) -> VideoSummary {
    VideoSummary {
        thumbnail_url: pick_thumbnail(&item.thumbnail, 336),
        channel: item.channel.map(|channel| channel.name),
        duration: item.duration.map(|seconds| Duration::from_secs(seconds.into())),
        views: item.view_count,
        age: item.publish_date_txt,
        is_short: item.is_short,
        video: VideoRef { source: ID, id: item.id },
        title: item.name,
    }
}

/// Smallest thumbnail at least `min_width` wide (sharp at the UI's size on a 2x display), else
/// the largest.
fn pick_thumbnail(thumbnails: &[Thumbnail], min_width: u32) -> Option<String> {
    thumbnails
        .iter()
        .filter(|thumbnail| thumbnail.width >= min_width)
        .min_by_key(|thumbnail| thumbnail.width)
        .or_else(|| thumbnails.iter().max_by_key(|thumbnail| thumbnail.width))
        .map(|thumbnail| thumbnail.url.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_youtube_links_and_ids() {
        assert!(claims_url("https://www.youtube.com/watch?v=aqz-KE-bpKQ"));
        assert!(claims_url("https://youtu.be/aqz-KE-bpKQ"));
        assert!(!claims_url("https://www.youtube.com.evil.example/watch"));
        assert!(!claims_url("https://vimeo.com/1"));
        assert!(is_video_id("aqz-KE-bpKQ"));
        assert!(!is_video_id("not an id"));
    }

    fn audio(itag: u64, track: Option<&str>) -> AudioStream {
        AudioStream {
            id: StreamId(itag),
            location: Location::new(format!("https://a/{itag}/{}", track.unwrap_or("-"))),
            codec: AudioCodec::Opus,
            bitrate: None,
            size: None,
            track: track.map(|id| AudioTrack { id: id.to_string(), name: id.to_string(), is_default: id == "en.4" }),
        }
    }

    #[test]
    fn dubs_get_their_own_ids() {
        let mut catalog = StreamCatalog {
            video: Vec::new(),
            audio: vec![
                audio(251, Some("en.4")),
                audio(140, Some("en.4")),
                audio(251, Some("de.3")),
                audio(140, Some("de.3")),
            ],
            resolver: "test",
            resolver_detail: String::new(),
            has_alternates: false,
        };
        number_tracks(&mut catalog);
        let ids: Vec<_> = catalog.audio.iter().map(|audio| audio.id).collect();
        assert_eq!(ids, [StreamId(251), StreamId(140), StreamId(251 | 1 << 32), StreamId(140 | 1 << 32)]);
        assert_eq!(catalog.audio_stream(ids[2]).map(|audio| audio.location.url.as_str()), Some("https://a/251/de.3"));
        assert_eq!(itag(ids[3]), 140);
        // No tracks: ids stay itags.
        let mut single = StreamCatalog { audio: vec![audio(251, None)], ..catalog };
        number_tracks(&mut single);
        assert_eq!(single.audio[0].id, StreamId(251));
    }

    /// Network test: `cargo test -- --ignored --nocapture live_catalog`
    #[test]
    #[ignore]
    fn live_catalog() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let preferences = StreamPreferences::default();
        for (first, id) in [
            (Resolver::VisionOs, "WhWc3b3KhnY"),
            (Resolver::VisionOs, "0e3GPea1Tyg"),
            (Resolver::RustyPipe, "0e3GPea1Tyg"),
        ] {
            let youtube = runtime.block_on(YouTube::new(reqwest::Client::new(), first, None)).expect("client");
            let catalog = runtime.block_on(youtube.streams(id.to_string(), preferences.clone())).expect("streams");
            println!("== {id} via {}: qualities {:?}", catalog.resolver_detail, catalog.qualities());
            let video = catalog.auto_video(&preferences, None).expect("auto video");
            assert!(video.resolution() <= preferences.max_height);
            let audio = catalog.audio_in_track(None, &preferences);
            assert_eq!(audio.first().map(|a| a.codec), Some(AudioCodec::Opus));
            let mut ids: Vec<_> = catalog.audio.iter().map(|audio| audio.id).collect();
            ids.sort_by_key(|id| id.0);
            ids.dedup();
            assert_eq!(ids.len(), catalog.audio.len(), "audio ids must be unique");
            for track in catalog.tracks().iter().filter(|track| !track.is_default).take(1) {
                let streams = catalog.audio_in_track(Some(&track.id), &preferences);
                assert!(streams.iter().all(|a| a.track.as_ref().map(|t| &t.id) == Some(&track.id)));
            }
        }
    }
}
