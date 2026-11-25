//! Pre-downloaded videos for development: `local:<name>` plays `<dir>/<name>/` without touching
//! the network, so work that isn't about a site doesn't spend its rate limits.
//! `scripts/fetch-dev-media.sh` fills the directory.
//!
//! Each video directory has a `manifest.json` (see [`Manifest`]) next to its stream files. The
//! files are served over HTTP on 127.0.0.1 with range requests, so the player takes the same path
//! as for a real site.

use std::collections::BTreeMap;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};

use super::media::{
    AudioCodec, AudioStream, Channel, Link, Location, Published, StreamCatalog, StreamId, VideoCodec, VideoDetails,
    VideoRef, VideoStream, VideoSummary,
};
use super::selection::StreamPreferences;
use super::source::{BoxFuture, VideoSource};

pub const ID: &str = "local";
const PREFIX: &str = "local:";

/// `<dir>/<name>/manifest.json`.
#[derive(Clone, Debug, Deserialize)]
struct Manifest {
    title: String,
    channel: Option<String>,
    #[serde(default)]
    description: String,
    views: Option<u64>,
    likes: Option<u64>,
    /// `YYYYMMDD`, as yt-dlp writes it.
    upload_date: Option<String>,
    /// Seconds.
    duration: Option<f64>,
    /// Image file in the video's directory.
    thumbnail: Option<String>,
    streams: Vec<ManifestStream>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum ManifestStream {
    Video {
        id: u64,
        file: String,
        codec: VideoCodec,
        width: u32,
        height: u32,
        #[serde(default)]
        fps: u8,
        /// e.g. "1080p"; derived from the size when missing.
        quality: Option<String>,
        bitrate: Option<u32>,
    },
    Audio {
        id: u64,
        file: String,
        codec: AudioCodec,
        bitrate: Option<u32>,
    },
}

struct Video {
    manifest: Manifest,
    dir: PathBuf,
}

pub struct Local {
    videos: BTreeMap<String, Video>,
    /// `http://127.0.0.1:<port>`, serving `<dir>`.
    base_url: String,
}

impl Local {
    /// Load every video under `dir` and start serving it. Must run inside the tokio runtime.
    /// `None` if `dir` has no usable videos.
    pub fn new(dir: &Path) -> Result<Option<Self>> {
        let mut videos = BTreeMap::new();
        let Ok(entries) = std::fs::read_dir(dir) else { return Ok(None) };
        for entry in entries.flatten() {
            let path = entry.path().join("manifest.json");
            let Ok(json) = std::fs::read(&path) else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            match serde_json::from_slice::<Manifest>(&json) {
                Ok(manifest) => {
                    videos.insert(name, Video { manifest, dir: entry.path() });
                }
                Err(err) => eprintln!("moth: skipping {}: {err}", path.display()),
            }
        }
        if videos.is_empty() {
            return Ok(None);
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").context("local source: failed to bind")?;
        listener.set_nonblocking(true)?;
        let base_url = format!("http://{}", listener.local_addr()?);
        let listener = TcpListener::from_std(listener)?;
        let root: Arc<Path> = dir.into();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, root.clone()));
            }
        });
        Ok(Some(Self { videos, base_url }))
    }

    fn video(&self, name: &str) -> Result<&Video> {
        self.videos.get(name).ok_or_else(|| anyhow!("no local video `{name}`"))
    }

    fn url(&self, name: &str, file: &str) -> String {
        format!("{}/{name}/{file}", self.base_url)
    }

    fn summary(&self, name: &str, video: &Video) -> VideoSummary {
        let manifest = &video.manifest;
        VideoSummary {
            video: VideoRef { source: ID, id: name.to_string() },
            title: manifest.title.clone(),
            channel: Some(format!("{} (local)", manifest.channel.as_deref().unwrap_or("local file"))),
            channel_id: None,
            channel_avatar_url: None,
            views: manifest.views,
            age: None,
            duration: manifest.duration.map(Duration::from_secs_f64),
            thumbnail_url: manifest.thumbnail.as_ref().map(|file| self.url(name, file)),
            description: None,
            is_short: false,
        }
    }

    fn catalog(&self, name: &str) -> Result<StreamCatalog> {
        let video = self.video(name)?;
        let size = |file: &str| std::fs::metadata(video.dir.join(file)).ok().map(|meta| meta.len());
        let mut catalog = StreamCatalog {
            video: Vec::new(),
            audio: Vec::new(),
            resolver: ID,
            resolver_detail: format!("local files ({})", video.dir.display()),
            has_alternates: false,
        };
        for stream in &video.manifest.streams {
            match stream.clone() {
                ManifestStream::Video { id, file, codec, width, height, fps, quality, bitrate } => {
                    let Some(size) = size(&file) else { continue };
                    catalog.video.push(VideoStream {
                        id: StreamId(id),
                        location: Location::new(self.url(name, &file)),
                        quality: quality.unwrap_or_else(|| format!("{}p", width.min(height))),
                        width,
                        height,
                        fps,
                        codec,
                        bitrate,
                        size: Some(size),
                        premium: false,
                    });
                }
                ManifestStream::Audio { id, file, codec, bitrate } => {
                    let Some(size) = size(&file) else { continue };
                    catalog.audio.push(AudioStream {
                        id: StreamId(id),
                        location: Location::new(self.url(name, &file)),
                        codec,
                        bitrate,
                        size: Some(size),
                        track: None,
                    });
                }
            }
        }
        if catalog.video.is_empty() || catalog.audio.is_empty() {
            bail!("local video `{name}` needs at least one video and one audio file");
        }
        Ok(catalog)
    }

    fn details(&self, name: &str) -> Result<VideoDetails> {
        let manifest = &self.video(name)?.manifest;
        let related = self
            .videos
            .iter()
            .filter(|(other, _)| other.as_str() != name)
            .map(|(other, video)| self.summary(other, video))
            .collect();
        Ok(VideoDetails {
            title: manifest.title.clone(),
            channel: Channel {
                id: None,
                name: manifest.channel.clone().unwrap_or_else(|| "local file".into()),
                avatar_url: None,
                subscribers: None,
            },
            views: manifest.views,
            likes: manifest.likes,
            published: manifest.upload_date.as_deref().and_then(parse_date),
            description: manifest.description.clone(),
            related,
        })
    }
}

fn parse_date(date: &str) -> Option<Published> {
    if date.len() != 8 {
        return None;
    }
    Some(Published::Date {
        year: date[..4].parse().ok()?,
        month: date[4..6].parse().ok()?,
        day: date[6..].parse().ok()?,
    })
}

/// `local:<name>`, optionally with `?t=<seconds>`.
fn parse_link(input: &str) -> Option<(&str, Option<Duration>)> {
    let rest = input.strip_prefix(PREFIX)?;
    let (name, query) = rest.split_once('?').unwrap_or((rest, ""));
    let start = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("t="))
        .and_then(|t| t.trim_end_matches('s').parse().ok())
        .map(Duration::from_secs);
    Some((name, start))
}

impl VideoSource for Local {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "local files"
    }

    fn claims(&self, input: &str) -> bool {
        input.starts_with(PREFIX)
    }

    fn resolve(&self, input: String) -> BoxFuture<Link> {
        let result = match parse_link(&input) {
            Some((name, start)) => {
                self.video(name).map(|_| Link { video: VideoRef { source: ID, id: name.to_string() }, start })
            }
            None => Err(anyhow!("not a local link: {input}")),
        };
        Box::pin(async move { result })
    }

    fn details(&self, id: String) -> BoxFuture<VideoDetails> {
        let result = self.details(&id);
        Box::pin(async move { result })
    }

    fn streams(&self, id: String, _preferences: StreamPreferences) -> BoxFuture<StreamCatalog> {
        let result = self.catalog(&id);
        Box::pin(async move { result })
    }

    fn web_url(&self, id: &str) -> String {
        format!("{PREFIX}{id}")
    }

    fn thumbnail_url(&self, id: &str) -> Option<String> {
        let file = self.videos.get(id)?.manifest.thumbnail.as_ref()?;
        Some(self.url(id, file))
    }

    fn featured(&self) -> Vec<VideoSummary> {
        self.videos.iter().map(|(name, video)| self.summary(name, video)).collect()
    }
}

// ── HTTP ─────────────────────────────────────────────────────────────────────

/// A minimal HTTP/1.1 file server: GET with an optional single `Range: bytes=a-b`, keep-alive.
async fn serve(stream: TcpStream, root: Arc<Path>) {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    loop {
        let Ok(Some((path, range))) = read_request(&mut read).await else { return };
        if respond(&mut write, &root, &path, range).await.is_err() {
            return;
        }
    }
}

type Range = (u64, Option<u64>);

/// The path and range of the next request; `None` when the client closed the connection.
async fn read_request(read: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Result<Option<(String, Option<Range>)>> {
    let mut line = String::new();
    if read.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (Some("GET"), Some(path)) = (parts.next(), parts.next()) else { bail!("unsupported request: {line}") };
    let path = path.to_string();
    let mut range = None;
    loop {
        line.clear();
        if read.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("range")
        {
            range = parse_range(value.trim());
        }
    }
    Ok(Some((path, range)))
}

fn parse_range(value: &str) -> Option<Range> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()))
}

async fn respond(
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    root: &Path,
    path: &str,
    range: Option<Range>,
) -> std::io::Result<()> {
    let file = match resolve_path(root, path) {
        Some(file) => tokio::fs::File::open(file).await.ok(),
        None => None,
    };
    let Some(mut file) = file else { return write_status(write, "404 Not Found", "").await };
    let size = file.metadata().await?.len();
    let (status, start, end, extra) = match range {
        None => ("200 OK", 0, size.saturating_sub(1), String::new()),
        Some((start, _)) if start >= size => {
            return write_status(write, "416 Range Not Satisfiable", &format!("Content-Range: bytes */{size}\r\n"))
                .await;
        }
        Some((start, end)) => {
            let end = end.unwrap_or(u64::MAX).min(size - 1);
            ("206 Partial Content", start, end, format!("Content-Range: bytes {start}-{end}/{size}\r\n"))
        }
    };
    let length = if size == 0 { 0 } else { end - start + 1 };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {length}\r\nAccept-Ranges: bytes\r\n{extra}Connection: keep-alive\r\n\r\n"
    );
    write.write_all(head.as_bytes()).await?;
    file.seek(SeekFrom::Start(start)).await?;
    tokio::io::copy(&mut file.take(length), write).await?;
    write.flush().await
}

async fn write_status(write: &mut tokio::net::tcp::OwnedWriteHalf, status: &str, extra: &str) -> std::io::Result<()> {
    let head = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n{extra}Connection: keep-alive\r\n\r\n");
    write.write_all(head.as_bytes()).await
}

/// `/<name>/<file>` under `root`; nothing else (no `..`, no nesting).
fn resolve_path(root: &Path, path: &str) -> Option<PathBuf> {
    let mut parts = path.strip_prefix('/')?.split('/');
    let (Some(name), Some(file), None) = (parts.next(), parts.next(), parts.next()) else { return None };
    let safe = |part: &str| !part.is_empty() && !part.starts_with('.') && !part.contains('\\');
    (safe(name) && safe(file)).then(|| root.join(name).join(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        assert_eq!(parse_link("local:spring"), Some(("spring", None)));
        assert_eq!(parse_link("local:spring?t=90"), Some(("spring", Some(Duration::from_secs(90)))));
        assert_eq!(parse_link("https://example.com"), None);
    }

    #[test]
    fn ranges_and_paths() {
        assert_eq!(parse_range("bytes=10-99"), Some((10, Some(99))));
        assert_eq!(parse_range("bytes=10-"), Some((10, None)));
        let root = Path::new("/media");
        assert_eq!(resolve_path(root, "/spring/248.webm"), Some(PathBuf::from("/media/spring/248.webm")));
        assert_eq!(resolve_path(root, "/spring/../x"), None);
        assert_eq!(resolve_path(root, "/../etc/passwd"), None);
        assert_eq!(resolve_path(root, "/spring"), None);
    }

    /// Serves real byte ranges over a socket, including keep-alive and a range past the end.
    #[test]
    fn serves_ranges() {
        let dir = std::env::temp_dir().join(format!("moth-local-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("clip")).unwrap();
        std::fs::write(dir.join("clip/a.bin"), (0u8..=255).collect::<Vec<_>>()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/clip/a.bin", listener.local_addr().unwrap());
            let root: Arc<Path> = dir.as_path().into();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(serve(stream, root.clone()));
                }
            });
            let client = reqwest::Client::new();
            let get = |range: &str| client.get(&url).header("Range", range.to_string()).send();
            let response = get("bytes=10-19").await.unwrap();
            assert_eq!(response.status(), 206);
            assert_eq!(response.bytes().await.unwrap().to_vec(), (10u8..20).collect::<Vec<_>>());
            let response = get("bytes=250-999").await.unwrap();
            assert_eq!(response.bytes().await.unwrap().len(), 6);
            assert_eq!(get("bytes=256-300").await.unwrap().status(), 416);
        });
        std::fs::remove_dir_all(dir).ok();
    }
}
