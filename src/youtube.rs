//! YouTube data and streams via RustyPipe (InnerTube), running on a small dedicated tokio runtime.
//! Everything returned here is already formatted for display, so the UI never sees RustyPipe types.

use std::future::Future;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use rustypipe::client::RustyPipe;
use rustypipe::model::richtext::ToPlaintext as _;
use rustypipe::model::{AudioCodec, AudioStream, Thumbnail, UrlTarget, VideoCodec, VideoItem, VideoPlayer};
use rustypipe::param::StreamFilter;

use crate::player::StreamUrls;

/// Preferred order: VP9 and AV1 are hardware-decodable on current GPUs; H.264 is the last resort.
const VIDEO_CODECS: [VideoCodec; 3] = [VideoCodec::Vp9, VideoCodec::Av01, VideoCodec::Avc1];

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

    /// Resolve playable stream URLs: best video-only stream up to `max_height` plus the best Opus audio.
    pub fn streams(&self, video_id: String, max_height: u32) -> impl Future<Output = Result<StreamUrls>> + use<> {
        let client = self.client.clone();
        self.run(async move {
            let player = client.query().player(&video_id).await?;
            select_streams(&player, max_height)
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

fn select_streams(player: &VideoPlayer, max_height: u32) -> Result<StreamUrls> {
    if player.details.is_live {
        bail!("Live streams aren't supported yet");
    }
    let video = VIDEO_CODECS
        .iter()
        .find_map(|codec| {
            let filter = StreamFilter::new().video_max_res(max_height).video_codecs([*codec]);
            player.select_video_only_stream(&filter)
        })
        .ok_or_else(|| anyhow!("no playable video stream"))?;
    // The iOS client also lists "stable volume" (DRC) re-encodes under the same itags. Skip them:
    // they're not what YouTube plays by default, and the regular files are byte-identical to
    // the web client's, which the player falls back to for seeking.
    let regular = |stream: &&AudioStream| !stream.url.contains("drc%3D1");
    let audio = [Some(AudioCodec::Opus), None]
        .into_iter()
        .find_map(|codec| {
            player
                .audio_streams
                .iter()
                .filter(regular)
                .filter(|stream| codec.is_none_or(|codec| stream.codec == codec))
                .max_by_key(|stream| stream.average_bitrate)
        })
        .ok_or_else(|| anyhow!("no playable audio stream"))?;
    Ok(StreamUrls {
        video: video.url.clone(),
        audio: audio.url.clone(),
        video_size: video.size,
        audio_size: Some(audio.size),
        video_bitrate: Some(video.average_bitrate),
        audio_bitrate: Some(audio.average_bitrate),
        video_itag: Some(video.itag),
        audio_itag: Some(audio.itag),
        width: Some(video.width),
        height: Some(video.height),
        info: format!(
            "{:?} {}x{}@{} · {:?} · via RustyPipe ({:?} client)",
            video.codec, video.width, video.height, video.fps, audio.codec, player.client_type
        )
        .to_lowercase(),
    })
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
