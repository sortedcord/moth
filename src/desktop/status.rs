//! What moth is doing ([`NowPlaying`]), shared between the UI (which publishes it), the socket
//! and MPRIS.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::Instant;

use serde::Serialize;

use crate::sync::lock;

/// The JSON reported by `moth status`. Field names and values are a public interface.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NowPlaying {
    pub running: bool,
    pub page: Page,
    pub status: PlaybackStatus,
    pub error: Option<String>,
    pub video: Option<VideoInfo>,
    /// Seconds.
    pub position: Option<f64>,
    pub duration: Option<f64>,
    pub rate: f64,
    pub stream: Option<StreamInfo>,
    pub fullscreen: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Page {
    None,
    Home,
    Watch,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackStatus {
    Idle,
    Resolving,
    Buffering,
    Playing,
    Paused,
    Ended,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VideoInfo {
    /// e.g. `youtube`.
    pub source: &'static str,
    pub id: String,
    pub url: Option<String>,
    /// Known once the watch page has loaded.
    pub title: Option<String>,
    pub channel: Option<String>,
    pub thumbnail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StreamInfo {
    /// Decoded frame size (or the stream's, before the first frame).
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// e.g. "1080p60".
    pub quality: Option<String>,
    /// Whether moth picks the quality (vs. one chosen in the menu).
    pub quality_auto: bool,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    /// The audio track's name (e.g. "German"), for videos with several (dubs).
    pub audio_track: Option<String>,
    /// GStreamer decoder element, e.g. "vavp9dec" (VA-API hardware).
    pub decoder: Option<String>,
    /// What found the streams, e.g. "rustypipe" or "yt-dlp".
    pub resolver: &'static str,
    /// Whether playback switched to alternate URLs (for seeking far ahead).
    pub fallback_urls: bool,
    /// Video is neither fetched nor decoded, because the window is hidden. Audio plays on.
    pub video_suspended: bool,
}

impl NowPlaying {
    pub fn not_running() -> Self {
        Self {
            running: false,
            page: Page::None,
            status: PlaybackStatus::Idle,
            error: None,
            video: None,
            position: None,
            duration: None,
            rate: 1.0,
            stream: None,
            fullscreen: false,
        }
    }

    /// Equal for reporting purposes: positions only count in whole seconds.
    fn same_as(&self, other: &Self) -> bool {
        let whole = |position: Option<f64>| position.map(|seconds| seconds.floor() as i64);
        whole(self.position) == whole(other.position)
            && Self { position: None, ..self.clone() } == Self { position: None, ..other.clone() }
    }

    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        line.push('\n');
        line
    }
}

/// The latest [`NowPlaying`], with the time it was captured so the position can be extrapolated.
pub struct Snapshot {
    pub now: NowPlaying,
    pub at: Instant,
}

impl Snapshot {
    /// Position right now: the captured one, advanced by the time since if playing.
    pub fn position(&self) -> Option<f64> {
        let position = self.now.position?;
        if self.now.status != PlaybackStatus::Playing {
            return Some(position);
        }
        let advanced = position + self.at.elapsed().as_secs_f64() * self.now.rate;
        Some(self.now.duration.map_or(advanced, |duration| advanced.min(duration)))
    }

    pub fn current(&self) -> NowPlaying {
        NowPlaying { position: self.position(), ..self.now.clone() }
    }
}

type Change = (NowPlaying, NowPlaying);

pub struct Hub {
    snapshot: Mutex<Snapshot>,
    /// `watch` clients of the socket.
    pub(super) subscribers: Mutex<Vec<UnixStream>>,
    changes: Mutex<Option<mpsc::Sender<Change>>>,
}

impl Hub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshot: Mutex::new(Snapshot {
                now: NowPlaying { running: true, ..NowPlaying::not_running() },
                at: Instant::now(),
            }),
            subscribers: Mutex::new(Vec::new()),
            changes: Mutex::new(None),
        })
    }

    pub fn snapshot(&self) -> MutexGuard<'_, Snapshot> {
        lock(&self.snapshot)
    }

    /// Record the current state. Cheap when nothing changed; changes are pushed to `watch`
    /// clients and MPRIS from a worker thread so the UI never waits on a slow client.
    pub fn publish(&self, now: NowPlaying) {
        let previous = std::mem::replace(&mut *lock(&self.snapshot), Snapshot { now: now.clone(), at: Instant::now() });
        if !previous.now.same_as(&now)
            && let Some(changes) = lock(&self.changes).as_ref()
        {
            // Report the old state extrapolated to now, so seek detection compares like with like.
            changes.send((previous.current(), now)).ok();
        }
    }

    /// Start the thread that forwards changes; `on_change` runs there (for MPRIS signals).
    pub fn start_worker(self: &Arc<Self>, mut on_change: impl FnMut(&NowPlaying, &NowPlaying) + Send + 'static) {
        let (sender, receiver) = mpsc::channel::<Change>();
        *lock(&self.changes) = Some(sender);
        let hub = self.clone();
        let spawned = std::thread::Builder::new().name("moth-status".into()).spawn(move || {
            while let Ok((previous, mut now)) = receiver.recv() {
                // Coalesce a backlog into one update.
                while let Ok((_, newer)) = receiver.try_recv() {
                    now = newer;
                }
                let line = now.to_line();
                lock(&hub.subscribers).retain_mut(|stream| stream.write_all(line.as_bytes()).is_ok());
                on_change(&previous, &now);
            }
        });
        if let Err(err) = spawned {
            eprintln!("moth: couldn't start the status thread: {err}");
        }
    }
}
