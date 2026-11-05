//! YouTube data and streams via RustyPipe (InnerTube), running on a small dedicated tokio runtime.
//! Everything returned here is already formatted for display, so the UI never sees RustyPipe types.

use std::future::Future;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use rustypipe::client::RustyPipe;
use rustypipe::model::richtext::ToPlaintext as _;
use rustypipe::model::{AudioCodec, AudioStream, Thumbnail, UrlTarget, VideoCodec, VideoItem, VideoPlayer};

use crate::player::StreamUrls;


pub struct WatchPage {
    pub title: String,
    pub channel: String,
    pub channel_avatar_url: Option<String>,
    pub subscribers: Option<String>,
    pub views: String,
    pub date: Option<String>,
    pub likes: Option<String>,
    pub description: String,
    pub related: Vec<RelatedVideo>,
}

pub struct RelatedVideo {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub meta: String,
    pub duration: Option<String>,
    pub thumbnail_url: Option<String>,
}

pub struct YouTube {
    runtime: tokio::runtime::Runtime,
    client: RustyPipe,
    http: reqwest::Client,
}

impl YouTube {
    pub fn new() -> Result<Arc<Self>> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("moth-net")
            .enable_all()
            .build()
            .context("failed to start tokio runtime")?;
        let storage_dir = cache_dir();
        std::fs::create_dir_all(&storage_dir)
            .with_context(|| format!("failed to create {}", storage_dir.display()))?;
        // RustyPipe's builder spawns background work, so it must be created inside the runtime.
        let client = runtime.block_on(async { RustyPipe::builder().storage_dir(storage_dir).build() })?;
        let http = reqwest::Client::builder().build()?;
        Ok(Arc::new(Self { runtime, client, http }))
    }

    /// Run `future` on the tokio runtime; the returned future can be awaited from gpui's executor.
    fn run<T, F>(&self, future: F) -> impl Future<Output = Result<T>> + use<T, F>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let handle = self.runtime.spawn(future);
        async move { handle.await.map_err(|err| anyhow!("background task failed: {err}"))? }
    }

    /// Turn a YouTube URL (watch, youtu.be, shorts, embed, music…) or bare video ID into a video ID.
    pub fn resolve_video_id(&self, input: String) -> impl Future<Output = Result<String>> + use<> {
        let client = self.client.clone();
        self.run(async move {
            let input = input.trim();
            if input.is_empty() {
                bail!("Enter a YouTube video link");
            }
            match client.query().resolve_string(input, false).await {
                Ok(UrlTarget::Video { id, .. }) => Ok(id),
                Ok(_) => bail!("Only video links are supported for now"),
                Err(_) => bail!("That doesn't look like a YouTube video link"),
            }
        })
    }

    pub fn watch_page(&self, video_id: String) -> impl Future<Output = Result<WatchPage>> + use<> {
        let client = self.client.clone();
        self.run(async move {
            let details = client.query().video_details(&video_id).await?;
            let channel = details.channel;
            Ok(WatchPage {
                title: details.name,
                channel_avatar_url: channel
                    .avatar
                    .iter()
                    .filter(|thumbnail| thumbnail.width >= 80)
                    .min_by_key(|thumbnail| thumbnail.width)
                    .or_else(|| channel.avatar.iter().max_by_key(|thumbnail| thumbnail.width))
                    .map(|thumbnail| thumbnail.url.clone()),
                subscribers: channel.subscriber_count.map(|n| format!("{} subscribers", compact_count(n))),
                channel: channel.name,
                views: format!("{} views", grouped_count(details.view_count)),
                date: details
                    .publish_date
                    .map(|date| format!("{} {}, {}", month_abbrev(u8::from(date.month())), date.day(), date.year()))
                    .or(details.publish_date_txt),
                likes: details.like_count.map(|n| compact_count(u64::from(n))),
                description: details.description.to_plaintext(),
                related: details.recommended.items.into_iter().filter(|item| !item.is_short).map(related_video).collect(),
            })
        })
    }

    /// Every playable stream of a video, plus the automatic pick (best video up to `max_height`,
    /// best Opus audio in the default track).
    pub fn streams(
        &self,
        video_id: String,
        max_height: u32,
    ) -> impl Future<Output = Result<(StreamUrls, StreamCatalog)>> + use<> {
        let client = self.client.clone();
        self.run(async move {
            let player = client.query().player(&video_id).await?;
            if player.details.is_live {
                bail!("Live streams aren't supported yet");
            }
            let catalog = StreamCatalog::from_player(&player);
            let video = catalog.auto_video(max_height, None).ok_or_else(|| anyhow!("no playable video stream"))?;
            let audio = catalog.auto_audio(None).ok_or_else(|| anyhow!("no playable audio stream"))?;
            let urls = catalog.urls(video, audio);
            Ok((urls, catalog))
        })
    }

    pub fn http(&self) -> crate::player::Http {
        crate::player::Http { client: self.http.clone(), runtime: self.runtime.handle().clone() }
    }

    pub fn fetch_bytes(&self, url: String) -> impl Future<Output = Result<Vec<u8>>> + use<> {
        let http = self.http.clone();
        self.run(async move {
            let response = http.get(&url).send().await?.error_for_status()?;
            Ok(response.bytes().await?.to_vec())
        })
    }
}

#[derive(Clone, Debug)]
pub struct VideoOption {
    pub itag: u32,
    pub url: String,
    /// YouTube's own label, e.g. "1080p60" (also for ultra-wide 2048x858 streams).
    pub quality: String,
    pub width: u32,
    pub height: u32,
    pub fps: u8,
    pub codec: &'static str,
    /// Average bitrate in bits per second.
    pub bitrate: u32,
    pub size: Option<u64>,
}

impl VideoOption {
    /// Resolution class as YouTube names it ("1080" for 1920x1080, 2048x858 and 1080x1920).
    pub fn resolution(&self) -> u32 {
        let digits: String = self.quality.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().unwrap_or_else(|_| self.width.min(self.height))
    }

    /// e.g. "1080p60", "720p".
    pub fn label(&self) -> String {
        match self.fps {
            fps if fps > 30 => format!("{}p{fps}", self.resolution()),
            _ => format!("{}p", self.resolution()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrackInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Debug)]
pub struct AudioOption {
    pub itag: u32,
    pub url: String,
    pub codec: &'static str,
    /// Average bitrate in bits per second.
    pub bitrate: u32,
    pub size: u64,
    pub track: Option<AudioTrackInfo>,
}

impl AudioOption {
    /// e.g. "Opus · 135 kbps".
    pub fn label(&self) -> String {
        format!("{} · {} kbps", self.codec, (self.bitrate + 500) / 1000)
    }
}

/// All streams of one video that moth can play.
#[derive(Clone, Debug, Default)]
pub struct StreamCatalog {
    pub video: Vec<VideoOption>,
    pub audio: Vec<AudioOption>,
    client: String,
}

/// Preference order: VP9 and AV1 are hardware-decodable on current GPUs; H.264 is the last resort.
const CODEC_ORDER: [&str; 3] = ["VP9", "AV1", "H.264"];

impl StreamCatalog {
    fn from_player(player: &VideoPlayer) -> Self {
        let video = player
            .video_only_streams
            .iter()
            .filter(|stream| !stream.hdr && stream.drm_track_type.is_none())
            .filter_map(|stream| {
                let codec = match stream.codec {
                    VideoCodec::Vp9 => "VP9",
                    VideoCodec::Av01 => "AV1",
                    VideoCodec::Avc1 => "H.264",
                    _ => return None,
                };
                Some(VideoOption {
                    itag: stream.itag,
                    url: stream.url.clone(),
                    quality: stream.quality.clone(),
                    width: stream.width,
                    height: stream.height,
                    fps: stream.fps,
                    codec,
                    bitrate: stream.average_bitrate,
                    size: stream.size,
                })
            })
            .collect();
        // The iOS client also lists "stable volume" (DRC) re-encodes under the same itags. Skip them:
        // they're not what YouTube plays by default, and the regular files are byte-identical to
        // the web client's, which the player falls back to for seeking.
        let audio = player
            .audio_streams
            .iter()
            .filter(|stream: &&AudioStream| !stream.url.contains("drc%3D1") && stream.drm_track_type.is_none())
            .filter_map(|stream| {
                let codec = match stream.codec {
                    AudioCodec::Opus => "Opus",
                    AudioCodec::Mp4a => "AAC",
                    _ => return None,
                };
                Some(AudioOption {
                    itag: stream.itag,
                    url: stream.url.clone(),
                    codec,
                    bitrate: stream.average_bitrate,
                    size: stream.size,
                    track: stream.track.as_ref().map(|track| AudioTrackInfo {
                        id: track.id.clone(),
                        name: track.lang_name.clone(),
                        is_default: track.is_default,
                    }),
                })
            })
            .collect();
        Self { video, audio, client: format!("{:?}", player.client_type).to_lowercase() }
    }

    fn codec_rank(codec: &str) -> usize {
        CODEC_ORDER.iter().position(|c| *c == codec).unwrap_or(CODEC_ORDER.len())
    }

    /// Best stream at or below `max_height`, preferring `codec` (if given) and then the usual
    /// codec order; if nothing is that small, the smallest stream.
    pub fn auto_video(&self, max_height: u32, codec: Option<&str>) -> Option<&VideoOption> {
        let rank = |option: &VideoOption| (Some(option.codec) != codec, Self::codec_rank(option.codec));
        let fitting = self.video.iter().filter(|option| option.resolution() <= max_height);
        let best_resolution = fitting.clone().map(|option| (option.resolution(), option.fps)).max();
        match best_resolution {
            Some(best) => fitting
                .filter(|option| (option.resolution(), option.fps) == best)
                .min_by_key(|option| rank(option)),
            None => self.video.iter().min_by_key(|option| (option.resolution(), rank(option))),
        }
    }

    /// Video options with the given resolution and frame rate, best codec first.
    pub fn video_variants(&self, resolution: u32, fps: u8) -> Vec<&VideoOption> {
        let mut variants: Vec<_> =
            self.video.iter().filter(|option| option.resolution() == resolution && option.fps == fps).collect();
        variants.sort_by_key(|option| Self::codec_rank(option.codec));
        variants
    }

    /// Distinct (resolution, fps) pairs, highest first.
    pub fn qualities(&self) -> Vec<(u32, u8)> {
        let mut qualities: Vec<_> = self.video.iter().map(|option| (option.resolution(), option.fps)).collect();
        qualities.sort_unstable_by(|a, b| b.cmp(a));
        qualities.dedup();
        qualities
    }

    pub fn tracks(&self) -> Vec<AudioTrackInfo> {
        let mut tracks: Vec<AudioTrackInfo> = Vec::new();
        for track in self.audio.iter().filter_map(|option| option.track.clone()) {
            if !tracks.iter().any(|known| known.id == track.id) {
                tracks.push(track);
            }
        }
        tracks.sort_by_key(|track| !track.is_default);
        tracks
    }

    /// Audio options in a track (the default track if `track_id` is `None`), best first.
    pub fn audio_in_track(&self, track_id: Option<&str>) -> Vec<&AudioOption> {
        let default_id = self.tracks().into_iter().find(|track| track.is_default).map(|track| track.id);
        let wanted = track_id.map(str::to_string).or(default_id);
        let mut options: Vec<_> = self
            .audio
            .iter()
            .filter(|option| wanted.is_none() || option.track.as_ref().map(|track| &track.id) == wanted.as_ref())
            .collect();
        options.sort_by_key(|option| (option.codec != "Opus", std::cmp::Reverse(option.bitrate)));
        options
    }

    /// Best audio (Opus preferred) in a track (the default track if `None`).
    pub fn auto_audio(&self, track_id: Option<&str>) -> Option<&AudioOption> {
        self.audio_in_track(track_id).into_iter().next()
    }

    pub fn video_by_itag(&self, itag: u32) -> Option<&VideoOption> {
        self.video.iter().find(|option| option.itag == itag)
    }

    pub fn audio_by_itag(&self, itag: u32) -> Option<&AudioOption> {
        self.audio.iter().find(|option| option.itag == itag)
    }

    pub fn urls(&self, video: &VideoOption, audio: &AudioOption) -> StreamUrls {
        StreamUrls {
            video: video.url.clone(),
            audio: audio.url.clone(),
            video_size: video.size,
            audio_size: Some(audio.size),
            video_bitrate: Some(video.bitrate),
            audio_bitrate: Some(audio.bitrate),
            video_itag: Some(video.itag),
            audio_itag: Some(audio.itag),
            width: Some(video.width),
            height: Some(video.height),
            info: format!(
                "{} {}x{}@{} · {} · via RustyPipe ({} client)",
                video.codec, video.width, video.height, video.fps, audio.label(), self.client
            ),
        }
    }
}

fn related_video(item: VideoItem) -> RelatedVideo {
    let meta = [
        item.view_count.map(|n| format!("{} views", compact_count(n))),
        item.publish_date_txt,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    RelatedVideo {
        thumbnail_url: pick_thumbnail(&item.thumbnail),
        channel: item.channel.map(|channel| channel.name).unwrap_or_default(),
        duration: item.duration.map(format_duration),
        id: item.id,
        title: item.name,
        meta,
    }
}

/// Smallest thumbnail that is still sharp at the list's 176px width on a 2x display.
fn pick_thumbnail(thumbnails: &[Thumbnail]) -> Option<String> {
    thumbnails
        .iter()
        .filter(|thumbnail| thumbnail.width >= 336)
        .min_by_key(|thumbnail| thumbnail.width)
        .or_else(|| thumbnails.iter().max_by_key(|thumbnail| thumbnail.width))
        .map(|thumbnail| thumbnail.url.clone())
}

fn cache_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("moth")
}

/// YouTube-style short counts: 999, 1.2K, 12K, 534K, 1.3M, 23M, 1.1B.
pub fn compact_count(n: u64) -> String {
    let (value, suffix) = match n {
        0..1_000 => return n.to_string(),
        1_000..1_000_000 => (n as f64 / 1e3, "K"),
        1_000_000..1_000_000_000 => (n as f64 / 1e6, "M"),
        _ => (n as f64 / 1e9, "B"),
    };
    if value < 10.0 {
        let rounded = (value * 10.0).floor() / 10.0;
        let text = format!("{rounded:.1}");
        format!("{}{suffix}", text.trim_end_matches(".0"))
    } else {
        format!("{}{suffix}", value.floor() as u64)
    }
}

fn grouped_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn format_duration(seconds: u32) -> String {
    let (h, m, s) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

fn month_abbrev(month: u8) -> &'static str {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    MONTHS.get(usize::from(month).saturating_sub(1)).copied().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network test: `cargo test --release -- --ignored --nocapture catalog`
    #[test]
    #[ignore]
    fn catalog_from_live_videos() {
        let youtube = YouTube::new().expect("client");
        for id in ["WhWc3b3KhnY", "0e3GPea1Tyg"] {
            let (urls, catalog) = youtube.runtime.block_on(youtube.streams(id.to_string(), 720)).expect("streams");
            println!("== {id}: auto = {}", urls.info);
            println!("   qualities: {:?}", catalog.qualities());
            let auto = catalog.auto_video(720, None).expect("auto video");
            assert!(auto.resolution() <= 720);
            let variants = catalog.video_variants(auto.resolution(), auto.fps);
            println!("   formats at {}: {:?}", auto.label(), variants.iter().map(|v| (v.codec, v.itag)).collect::<Vec<_>>());
            let tracks = catalog.tracks();
            println!("   audio tracks ({}): {:?}", tracks.len(), tracks.iter().take(6).map(|t| (&t.name, t.is_default)).collect::<Vec<_>>());
            let audio = catalog.audio_in_track(None);
            println!("   audio (default track): {:?}", audio.iter().map(|a| a.label()).collect::<Vec<_>>());
            assert_eq!(audio.first().map(|a| a.codec), Some("Opus"));
            if let Some(other) = tracks.iter().find(|t| !t.is_default) {
                let options = catalog.audio_in_track(Some(&other.id));
                assert!(!options.is_empty() && options.iter().all(|a| a.track.as_ref().map(|t| &t.id) == Some(&other.id)));
                println!("   audio ({}): {:?}", other.name, options.iter().map(|a| a.label()).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn counts() {
        assert_eq!(compact_count(999), "999");
        assert_eq!(compact_count(1_250), "1.2K");
        assert_eq!(compact_count(12_900), "12K");
        assert_eq!(compact_count(534_000), "534K");
        assert_eq!(compact_count(1_000_000), "1M");
        assert_eq!(compact_count(23_507_910), "23M");
        assert_eq!(grouped_count(10_967_424), "10,967,424");
        assert_eq!(grouped_count(999), "999");
        assert_eq!(format_duration(464), "7:44");
        assert_eq!(format_duration(3725), "1:02:05");
    }
}
