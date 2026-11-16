//! Pipeline internals for the player debug panel.

use std::sync::Mutex;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;

use super::http_source::{Alternate, Endpoint};
use crate::sync::lock;

pub struct PlayerStats {
    pub clock: String,
    /// e.g. "Playing", or "Paused → Playing" while changing.
    pub state: String,
    pub position: Option<Duration>,
    /// Pipeline running time (clock time minus base time).
    pub running: Option<Duration>,
    pub rate: f64,
    /// Negotiated caps at the sinks, e.g. "1920x1080 @ 24/1 · NV12 (memory:DMABuf)".
    pub video_caps: Option<String>,
    pub audio_caps: Option<String>,
    pub video_sink: SinkStats,
    pub audio_sink: SinkStats,
    /// Decoded data waiting in front of each sink.
    pub video_queue: QueueLevel,
    pub audio_queue: QueueLevel,
    /// Last rendered video timestamp minus last rendered audio timestamp, in milliseconds.
    pub av_offset_ms: Option<f64>,
    pub video_net: NetStats,
    pub audio_net: NetStats,
    /// Video is neither fetched nor decoded (see [`super::Player::set_video_suspended`]).
    pub video_suspended: bool,
}

/// GStreamer resets these on every pause/resume.
#[derive(Clone, Copy, Default)]
pub struct SinkStats {
    pub rendered: u64,
    pub dropped: u64,
}

#[derive(Clone, Copy, Default)]
pub struct QueueLevel {
    pub buffers: u32,
    pub bytes: u32,
    pub time: Duration,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlternateState {
    /// The stream has no alternate URL.
    #[default]
    None,
    Resolving,
    Ready,
    /// Playing from the alternate URL.
    Switched,
}

/// Download counters for one stream, kept by the HTTP source.
#[derive(Clone, Default)]
pub struct NetStats {
    pub host: String,
    pub bytes: u64,
    pub requests: u64,
    /// Refused requests (HTTP 403).
    pub forbidden: u64,
    /// Throughput of the most recent request, bytes per second.
    pub last_rate: f64,
    pub last_request: Duration,
    /// Bytes queued in the appsrc, not yet demuxed.
    pub queued: u64,
    pub alternate: AlternateState,
}

pub(super) fn sink_stats(sink: &gst::Element) -> SinkStats {
    let stats = sink.property::<gst::Structure>("stats");
    SinkStats { rendered: stats.get("rendered").unwrap_or(0), dropped: stats.get("dropped").unwrap_or(0) }
}

pub(super) fn queue_level(queue: &gst::Element) -> QueueLevel {
    QueueLevel {
        buffers: queue.property("current-level-buffers"),
        bytes: queue.property("current-level-bytes"),
        time: Duration::from_nanos(queue.property("current-level-time")),
    }
}

pub(super) fn last_sample_pts(sink: &gst::Element) -> Option<u64> {
    let sample = sink.property::<Option<gst::Sample>>("last-sample")?;
    Some(sample.buffer()?.pts()?.nseconds())
}

/// e.g. "1920x1080 @ 24/1 · NV12 (memory:DMABuf)" or "48000 Hz · 2 ch · F32LE".
pub(super) fn pad_caps(sink: &gst::Element) -> Option<String> {
    let caps = sink.static_pad("sink")?.current_caps()?;
    let structure = caps.structure(0)?;
    let mut parts = Vec::new();
    if let (Ok(w), Ok(h)) = (structure.get::<i32>("width"), structure.get::<i32>("height")) {
        let fps = structure.get::<gst::Fraction>("framerate").map(|f| format!(" @ {}/{}", f.numer(), f.denom()));
        parts.push(format!("{w}x{h}{}", fps.unwrap_or_default()));
    }
    if let Ok(rate) = structure.get::<i32>("rate") {
        parts.push(format!("{rate} Hz"));
    }
    if let Ok(channels) = structure.get::<i32>("channels") {
        parts.push(format!("{channels} ch"));
    }
    if let Ok(format) = structure.get::<&str>("drm-format").or_else(|_| structure.get::<&str>("format")) {
        parts.push(format.to_string());
    }
    if let Some(features) = caps.features(0).filter(|features| !features.is_any() && features.size() > 0) {
        parts.push(format!("({features})"));
    }
    Some(parts.join(" · "))
}

pub(super) fn net_stats(endpoint: &Mutex<Endpoint>, appsrc: &gst::Element) -> NetStats {
    let endpoint = lock(endpoint);
    let mut stats = endpoint.net.clone();
    stats.host = reqwest::Url::parse(&endpoint.location.url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default();
    stats.queued = appsrc.property("current-level-bytes");
    stats.alternate = match (&endpoint.alternate, endpoint.switched) {
        (_, true) => AlternateState::Switched,
        (Alternate::Pending, _) => AlternateState::Resolving,
        (Alternate::Ready(_), _) => AlternateState::Ready,
        (Alternate::Unavailable, _) => AlternateState::None,
    };
    stats
}
