//! yt-dlp as a stream resolver: the fallback when RustyPipe fails, and the source of web-client
//! URLs for seeking (see [`super::YouTube::alternate_urls`]).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;

use crate::backend::media::{AudioCodec, AudioStream, Location, StreamCatalog, StreamId, VideoCodec, VideoStream};
use crate::backend::selection::StreamPreferences;
use crate::backend::tools;

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
    /// Every format yt-dlp found (only printed when no format was asked for, see [`premium`]).
    formats: Option<Vec<Format>>,
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
    /// Unix time from which YouTube serves the URL (see [`Resolved::usable_in`]).
    available_at: Option<f64>,
    /// `https` for plain files, `m3u8_native` and the like for playlists.
    protocol: Option<String>,
}

impl Format {
    fn location(&mut self) -> Location {
        Location {
            url: std::mem::take(&mut self.url),
            headers: std::mem::take(&mut self.http_headers).into_iter().collect(),
        }
    }
}

/// The longest moth waits for URLs to become usable: longer than the ads YouTube schedules, short
/// enough not to look hung if the time yt-dlp reports is ever nonsense.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// What yt-dlp resolved.
pub struct Resolved {
    pub catalog: StreamCatalog,
    available_at: Option<SystemTime>,
}

impl Resolved {
    /// How long until YouTube serves the catalog's URLs.
    ///
    /// When YouTube schedules an ad before the video, its servers refuse the streams (HTTP 403)
    /// until the ad, or the part of it that can't be skipped, would have played: a few seconds,
    /// or none at all, which made far seeks fail every now and then. yt-dlp reports that time as
    /// `available_at` and sits it out before a download, but asked for JSON it only prints it.
    pub fn usable_in(&self) -> Duration {
        self.usable_from(SystemTime::now())
    }

    fn usable_from(&self, now: SystemTime) -> Duration {
        let wait = self.available_at.and_then(|available_at| available_at.duration_since(now).ok());
        wait.unwrap_or_default().min(MAX_WAIT)
    }
}

/// Resolve one video+audio pair, signed in with the session in `cookies` (a Netscape cookies
/// file) if given. Blocking; run it on a blocking thread.
pub fn resolve(video_id: &str, format: &str, cookies: Option<&Path>) -> Result<Resolved> {
    // yt-dlp may run alongside playback (for alternate URLs), so keep it out of the way.
    let mut command = tools::ytdlp_command();
    command.args(["--no-warnings", "-f", format, "-j"]);
    if let Some(cookies) = cookies {
        command.arg("--cookies").arg(cookies);
    }
    let output = command.args(["--", video_id]).output().context("failed to run yt-dlp")?;
    if !output.status.success() {
        bail!("yt-dlp: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    parse(&output.stdout)
}

/// The Premium-only streams of a video, for the session in `cookies` (a Netscape cookies file):
/// the enhanced-bitrate video (1080p Premium) and the 256 kbps audio YouTube Music offers. The
/// catalog has whichever of them the video has, and may be empty. Blocking; run it on a
/// blocking thread.
///
/// yt-dlp gets these from YouTube's TV client, signed in with the session, as plain HTTPS
/// URLs: Premium formats don't need SABR after all (#17).
pub fn premium(video_id: &str, cookies: &Path) -> Result<Resolved> {
    let output = tools::ytdlp_command()
        .args(["--no-playlist", "--ignore-no-formats-error", "-J", "--cookies"])
        .arg(cookies)
        .args(["--", video_id])
        .output()
        .context("failed to run yt-dlp")?;
    if !output.status.success() {
        bail!("yt-dlp: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let resolved = parse_premium(&output.stdout)?;
    if resolved.catalog.video.is_empty() && resolved.catalog.audio.is_empty() {
        // Without formats at all yt-dlp only warns, e.g. when it can't solve YouTube's
        // n challenge (an outdated yt-dlp): say why there are none.
        let warnings: Vec<_> = String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|line| line.starts_with("WARNING") || line.starts_with("ERROR"))
            .map(|line| line.chars().take(200).collect::<String>())
            .collect();
        if !warnings.is_empty() {
            eprintln!(
                "moth: yt-dlp warned while looking for Premium streams (is it up to date?):\n  {}",
                warnings.join("\n  ")
            );
        }
    }
    Ok(resolved)
}

/// iTags of the 256 kbps audio (AAC and Opus), which are plain files like any other, only
/// offered to Premium accounts.
const PREMIUM_AUDIO: [&str; 2] = ["141", "774"];

/// Pick the Premium streams out of `yt-dlp -J`.
fn parse_premium(json: &[u8]) -> Result<Resolved> {
    let info: Info = serde_json::from_slice(json).context("unexpected yt-dlp output")?;
    let formats = info.formats.unwrap_or_default();
    let available_at = latest_available_at(&formats);
    let has = |codec: &Option<String>| codec.as_deref().is_some_and(|codec| codec != "none");
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for format in formats {
        if format.protocol.as_deref() != Some("https") {
            continue;
        }
        if has(&format.vcodec) && format.format_note.as_deref().is_some_and(|note| note.contains("Premium")) {
            // An unsupported codec only drops that stream.
            video.extend(video_stream(format).ok());
        } else if has(&format.acodec) && !has(&format.vcodec) && PREMIUM_AUDIO.contains(&format.format_id.as_str()) {
            audio.extend(audio_stream(format).ok());
        }
    }
    let catalog = StreamCatalog {
        video,
        audio,
        resolver: "yt-dlp",
        resolver_detail: "yt-dlp (Premium)".to_string(),
        has_alternates: false,
    };
    Ok(Resolved { catalog, available_at })
}

fn latest_available_at(formats: &[Format]) -> Option<SystemTime> {
    formats
        .iter()
        .filter_map(|format| format.available_at)
        .reduce(f64::max)
        .and_then(|at| Duration::try_from_secs_f64(at).ok())
        .map(|at| SystemTime::UNIX_EPOCH + at)
}

/// A session's cookies as a Netscape cookies file for yt-dlp: private to the user, in the
/// runtime directory, and deleted when dropped.
pub struct CookieFile(std::path::PathBuf);

impl CookieFile {
    /// From a `Cookie` header value (`name=value; …`), for `.youtube.com`.
    pub fn new(header: &str) -> Result<Self> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = crate::paths::runtime_dir().unwrap_or_else(std::env::temp_dir);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.join(format!("moth-cookies-{}-{n}.txt", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("couldn't create {}", path.display()))?;
        let file_and_path = Self(path);
        file.write_all(netscape_cookies(header).as_bytes())?;
        Ok(file_and_path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for CookieFile {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

/// `name=value; …` → Netscape cookie file lines for `.youtube.com`, valid for a year.
fn netscape_cookies(header: &str) -> String {
    let expires = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs() + 365 * 86_400;
    let mut file = String::from("# Netscape HTTP Cookie File\n");
    for (name, value) in header.split(';').filter_map(|pair| pair.trim().split_once('=')) {
        file.push_str(&format!(".youtube.com\tTRUE\t/\tTRUE\t{expires}\t{name}\t{value}\n"));
    }
    file
}

/// Read what `yt-dlp -j` printed.
fn parse(json: &[u8]) -> Result<Resolved> {
    let info: Info = serde_json::from_slice(json).context("unexpected yt-dlp output")?;
    let formats = info
        .requested_formats
        .ok_or_else(|| anyhow!("yt-dlp picked a combined stream; moth needs separate video and audio"))?;
    let available_at = latest_available_at(&formats);
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
    let catalog = StreamCatalog {
        video,
        audio,
        resolver: "yt-dlp",
        resolver_detail: "yt-dlp".to_string(),
        has_alternates: false,
    };
    Ok(Resolved { catalog, available_at })
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
    let premium = format.format_note.as_deref().is_some_and(|note| note.contains("Premium"));
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
        premium,
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
    fn writes_netscape_cookies() {
        let file = netscape_cookies("SID=abc; __Secure-3PSID=d=e");
        let lines: Vec<&str> = file.lines().collect();
        assert_eq!(lines[0], "# Netscape HTTP Cookie File");
        let fields: Vec<&str> = lines[2].split('\t').collect();
        assert_eq!((fields[0], fields[5], fields[6]), (".youtube.com", "__Secure-3PSID", "d=e"));
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn format_groups_alternatives() {
        let format = format_for(&StreamPreferences::default());
        assert!(format.starts_with("(bv[height<=720][vcodec^=vp9][protocol=https]/"));
        assert!(format.ends_with(")+(ba[acodec^=opus][protocol=https]/ba[protocol=https])"));
        assert_eq!(stream_id("251-drc"), StreamId(251));
    }

    /// The fields moth reads from `yt-dlp -j`, for one video and one audio format.
    fn output(video_extra: &str, audio_extra: &str) -> String {
        format!(
            r#"{{"id": "eRsGyueVLvQ", "epoch": 1794821410, "requested_formats": [
                {{"format_id": "247", "url": "https://rr4.example/videoplayback?itag=247", "vcodec": "vp9",
                  "acodec": "none", "width": 1280, "height": 546, "fps": 24, "tbr": 587.312,
                  "filesize": 65194674, "format_note": "720p", "http_headers": {{"User-Agent": "Mozilla/5.0"}}
                  {video_extra}}},
                {{"format_id": "251", "url": "https://rr4.example/videoplayback?itag=251", "vcodec": "none",
                  "acodec": "opus", "tbr": 129.725, "filesize": 14400537, "format_note": "medium"
                  {audio_extra}}}
            ]}}"#
        )
    }

    #[test]
    fn reads_formats_and_headers() {
        let resolved = parse(output("", "").as_bytes()).expect("parses");
        let (video, audio) = (&resolved.catalog.video[0], &resolved.catalog.audio[0]);
        assert_eq!((video.id, video.codec, video.size), (StreamId(247), VideoCodec::Vp9, Some(65_194_674)));
        assert_eq!(video.location.headers, [("User-Agent".to_string(), "Mozilla/5.0".to_string())]);
        assert_eq!((audio.id, audio.codec, audio.bitrate), (StreamId(251), AudioCodec::Opus, Some(129_725)));
        assert_eq!(resolved.usable_in(), Duration::ZERO);
    }

    #[test]
    fn picks_premium_streams_from_all_formats() {
        let json = r#"{"formats": [
            {"format_id": "399", "protocol": "https", "url": "https://x/399", "vcodec": "av01.0.08M.08",
             "acodec": "none", "width": 1920, "height": 1080, "fps": 24, "tbr": 1142, "format_note": "1080p"},
            {"format_id": "721", "protocol": "https", "url": "https://x/721", "vcodec": "av01.0.09M.08",
             "acodec": "none", "width": 1920, "height": 1080, "fps": 60, "tbr": 4894.95,
             "filesize": 350090532, "format_note": "1080p60 Premium",
             "http_headers": {"User-Agent": "Mozilla/5.0"}},
            {"format_id": "616", "protocol": "m3u8_native", "url": "https://x/616", "vcodec": "vp09.00.40.08",
             "acodec": "none", "width": 1920, "height": 1080, "fps": 24, "format_note": "1080p Premium"},
            {"format_id": "251", "protocol": "https", "url": "https://x/251", "vcodec": "none",
             "acodec": "opus", "tbr": 129, "format_note": "medium"},
            {"format_id": "774", "protocol": "https", "url": "https://x/774", "vcodec": "none",
             "acodec": "opus", "tbr": 257, "format_note": "high"},
            {"format_id": "774-drc", "protocol": "https", "url": "https://x/774d", "vcodec": "none",
             "acodec": "opus", "tbr": 257, "format_note": "high, DRC"}
        ]}"#;
        let catalog = parse_premium(json.as_bytes()).expect("parses").catalog;
        let video: Vec<_> = catalog.video.iter().map(|stream| (stream.id, stream.premium, stream.fps)).collect();
        assert_eq!(video, [(StreamId(721), true, 60)]);
        assert_eq!(catalog.video[0].location.headers, [("User-Agent".to_string(), "Mozilla/5.0".to_string())]);
        let audio: Vec<_> = catalog.audio.iter().map(|stream| (stream.id, stream.codec)).collect();
        assert_eq!(audio, [(StreamId(774), AudioCodec::Opus)]);
        // A video without any gives an empty catalog, not an error.
        assert!(parse_premium(br#"{"formats": []}"#).expect("parses").catalog.video.is_empty());
    }

    #[test]
    fn waits_until_every_url_is_available() {
        let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        let json = output(r#", "available_at": 1794821412"#, r#", "available_at": 1794821415"#);
        let resolved = parse(json.as_bytes()).expect("parses");
        assert_eq!(resolved.usable_from(at(1_794_821_410)), Duration::from_secs(5));
        assert_eq!(resolved.usable_from(at(1_794_821_415)), Duration::ZERO);
        assert_eq!(resolved.usable_from(at(1_794_821_500)), Duration::ZERO);
        // A time that can't be right doesn't stall playback for long.
        assert_eq!(resolved.usable_from(at(1_794_000_000)), MAX_WAIT);
    }
}
