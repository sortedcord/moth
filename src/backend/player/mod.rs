//! Video playback: a GStreamer pipeline whose video sink (`waylandsink`) renders into a
//! `wl_subsurface` of the app window. Decoded frames (VA-API dmabufs) go straight to the
//! compositor; moth never touches video pixels.
//!
//! ```text
//! http appsrc → decodebin3 → queue → waylandsink
//! http appsrc → decodebin3 → queue → audioconvert → volume → scaletempo → audioconvert → audioresample → pulsesink
//! ```

mod http_source;
mod mp4_index;
pub mod stats;
mod wayland;
mod webm_index;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use futures::{Stream, StreamExt as _};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;

use super::Http;
use super::media::{AlternateUrls, Location, Selection, StreamCatalog};
use crate::sync::lock;
use http_source::{Alternate, Endpoint, Flow};
pub use stats::PlayerStats;
pub use wayland::WaylandTarget;

pub fn init() -> Result<()> {
    gst::init().context("failed to initialize GStreamer")
}

/// Where the video goes, in surface-local logical pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    /// Placeholder until the UI has laid the video out: one pixel in the corner.
    pub const HIDDEN: Self = Self { x: 0, y: 0, w: 1, h: 1 };
}

/// One stream to fetch.
#[derive(Clone, Debug)]
pub struct StreamInput {
    pub location: Location,
    /// Exact length in bytes, if known.
    pub size: Option<u64>,
    /// Average bitrate, bits per second; sizes the range requests.
    pub bitrate: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct PlayerInput {
    /// `None` plays the audio alone (no video is fetched or decoded).
    pub video: Option<StreamInput>,
    pub audio: StreamInput,
    /// Alternate URLs are being resolved and will arrive through [`Player::set_alternates`] (or
    /// [`Player::alternates_failed`]); until then a refused request waits for them.
    pub alternates_pending: bool,
    /// The URLs may refuse to serve far ahead of playback, as the catalog's alternates exist to
    /// work around (see [`StreamCatalog::has_alternates`]).
    pub near_only: bool,
}

impl PlayerInput {
    pub fn new(catalog: &StreamCatalog, selection: Selection, alternates_pending: bool) -> Option<Self> {
        let video = match selection.video {
            Some(id) => Some(catalog.video_stream(id)?),
            None => None,
        };
        let audio = catalog.audio_stream(selection.audio)?;
        Some(Self {
            video: video.map(|video| StreamInput {
                location: video.location.clone(),
                size: video.size,
                bitrate: video.bitrate,
            }),
            audio: StreamInput { location: audio.location.clone(), size: audio.size, bitrate: audio.bitrate },
            alternates_pending,
            near_only: catalog.has_alternates,
        })
    }
}

/// Where a new pipeline begins (switching streams resumes mid-video).
#[derive(Clone, Copy, Debug)]
pub struct Start {
    pub position: Duration,
    pub rate: f64,
    pub paused: bool,
}

/// Pass every event to [`Player::handle`] before acting on it.
#[derive(Clone, Debug, PartialEq)]
pub enum PlayerEvent {
    Error(String),
    Eos,
    /// The pipeline finished an asynchronous state change or seek.
    AsyncDone,
    /// A stream's source has seen the beginning of its stream, and the player can start it.
    SourceReady(Source),
    Playing,
    Paused,
}

/// One of the player's stream sources.
#[derive(Clone, Debug, PartialEq)]
pub struct Source(gst::Element);

/// The decoded video's format, once caps are negotiated.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    pub pixel_aspect: f32,
}

impl VideoFormat {
    /// Width / height as displayed.
    pub fn display_aspect(&self) -> Option<f32> {
        (self.width > 0 && self.height > 0).then(|| self.width as f32 * self.pixel_aspect / self.height as f32)
    }
}

/// Positions the video subsurface. Cheap to clone; usable from paint code.
#[derive(Clone)]
pub struct VideoOverlay(gst::Element);

impl VideoOverlay {
    pub fn set_rect(&self, rect: Rect) {
        if let Some(overlay) = self.0.dynamic_cast_ref::<gst_video::VideoOverlay>() {
            overlay.set_render_rectangle(rect.x, rect.y, rect.w, rect.h).ok();
            overlay.expose();
        }
    }
}

/// The player starts straight into PLAYING and holds any seek, rate or pause requested before
/// then (`pending_position`, `rate`, `paused`), applying it once PLAYING is reached. Flushing
/// (seeking, or changing the rate, which is a seek) before the first PLAYING deadlocked with
/// pipewiresink, moth's earlier audio sink (pipewire#5171, <https://gitlab.freedesktop.org/pipewire/pipewire/-/work_items/5171>);
/// it also keeps the first audible output from being at the wrong position.
pub struct Player {
    pipeline: gst::Pipeline,
    /// `None` when playing audio alone.
    video: Option<VideoBranch>,
    audio_sink: gst::Element,
    volume: gst::Element,
    audio_queue: gst::Element,
    audio_src: gst::Element,
    paused: bool,
    rate: f64,
    /// Whether the pipeline has reached PLAYING (see the type's docs).
    started: bool,
    /// Where to seek once started; `None` to stay at the beginning.
    pending_position: Option<Duration>,
    /// Muted until the start seek lands, so a mid-video start doesn't blip the opening.
    unmute_on_async_done: bool,
    audio_endpoint: Arc<Mutex<Endpoint>>,
    /// The audio has played to the end (until the next seek).
    audio_ended: Arc<AtomicBool>,
    near_only: bool,
}

impl Player {
    /// Build and start the pipeline, drawing into `target` at `rect`. Returns the player and its
    /// events; the stream ends when the player is dropped.
    pub fn new(
        input: &PlayerInput,
        start: Option<Start>,
        target: &WaylandTarget,
        rect: Rect,
        http: &Http,
    ) -> Result<(Self, impl Stream<Item = PlayerEvent> + Unpin + Send + use<>)> {
        let make = |name: &str| {
            gst::ElementFactory::make(name).build().with_context(|| format!("missing GStreamer element `{name}`"))
        };
        let audio_endpoint = Endpoint::new(input.audio.location.clone(), input.alternates_pending);
        let pipeline = gst::Pipeline::new();
        let video = match &input.video {
            Some(video) => Some(VideoBranch::new(&pipeline, video, input.alternates_pending, target, rect, http)?),
            None => None,
        };

        let asrc = http_source::add_to(
            &pipeline,
            http.clone(),
            audio_endpoint.clone(),
            input.audio.size,
            input.audio.bitrate,
        )?;
        let adecode = make("decodebin3")?;
        let aqueue = make("queue")?;
        let aconvert = make("audioconvert")?;
        let volume = make("volume")?;
        // Keeps the pitch natural at other speeds.
        let tempo = make("scaletempo")?;
        let aconvert_out = make("audioconvert")?;
        let aresample = make("audioresample")?;
        // The audio sink is the pipeline's clock, as in playbin: the audio device plays at its own
        // rate, and video follows. Making the sink follow another clock instead means correcting
        // the difference by editing the audio: GstAudioBaseSink's "skew" slaving jumps the
        // playback pointer (a pop each time, several during start-up), and pipewiresink's
        // "resample" dropped ~16 ms every second or two.
        // pulsesink (served by pipewire-pulse) rather than pipewiresink, because pipewiresink's clock
        // jumps when the idle device wakes up, so resuming after a pause skipped ahead by up to the
        // pause length. pulsesink's clock stops while paused.
        let asink = make("pulsesink")?;

        pipeline.add_many([&adecode, &aqueue, &aconvert, &volume, &tempo, &aconvert_out, &aresample, &asink])?;
        asrc.link(&adecode)?;
        gst::Element::link_many([&aqueue, &aconvert, &volume, &tempo, &aconvert_out, &aresample, &asink])?;
        link_decoded_pad(&adecode, &aqueue, "audio_");

        let audio_ended = Arc::new(AtomicBool::new(false));
        watch_audio_end(
            &asink,
            audio_ended.clone(),
            video.as_ref().map(|video| (video.endpoint.clone(), video.sink.clone())),
        );

        let pending_position = start.map(|start| start.position).filter(|position| !position.is_zero());
        volume.set_property("mute", pending_position.is_some());
        let events = events(&pipeline)?;
        pipeline.set_state(gst::State::Playing).context("failed to start pipeline")?;

        let player = Self {
            pipeline,
            video,
            audio_sink: asink,
            volume,
            audio_queue: aqueue,
            audio_src: asrc,
            paused: start.is_some_and(|start| start.paused),
            rate: start.map_or(1.0, |start| start.rate),
            started: false,
            pending_position,
            unmute_on_async_done: false,
            audio_endpoint,
            audio_ended,
            near_only: input.near_only,
        };
        Ok((player, events))
    }

    /// Keep the player's own state machine going; call with every event.
    pub fn handle(&mut self, event: &PlayerEvent) {
        match event {
            PlayerEvent::Playing if !self.started => {
                self.started = true;
                let position = self.pending_position.take();
                if position.is_some() || self.rate != 1.0 {
                    self.unmute_on_async_done = position.is_some();
                    self.flush_seek(position.or_else(|| self.position()).unwrap_or_default());
                }
                if self.paused {
                    self.pipeline.set_state(gst::State::Paused).ok();
                }
            }
            PlayerEvent::SourceReady(source) => {
                if let Err(err) = http_source::start(&source.0) {
                    eprintln!("moth: {err:#}");
                }
            }
            PlayerEvent::AsyncDone if self.unmute_on_async_done => {
                self.unmute_on_async_done = false;
                self.volume.set_property("mute", false);
            }
            _ => {}
        }
    }

    /// Where the video is drawn; `None` when playing audio alone.
    pub fn overlay(&self) -> Option<VideoOverlay> {
        self.video.as_ref().map(|video| VideoOverlay(video.sink.clone()))
    }

    pub fn has_video(&self) -> bool {
        self.video.is_some()
    }

    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
        if let Some(position) = self.position() {
            self.seek(position);
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Accurate seek, keeping the rate. Before the pipeline has started, it's applied on start.
    pub fn seek(&mut self, position: Duration) {
        if self.started {
            self.flush_seek(position);
        } else {
            self.pending_position = Some(position);
        }
    }

    fn flush_seek(&self, position: Duration) {
        let result = self.pipeline.seek(
            self.rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            Some(gst::ClockTime::from_nseconds(position.as_nanos() as u64)),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
        if let Err(err) = result {
            eprintln!("moth: seek to {position:?} failed: {err}");
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if !self.started {
            return;
        }
        let target = if paused { gst::State::Paused } else { gst::State::Playing };
        self.pipeline.set_state(target).ok();
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// The pipeline has started and reached the [`Start`] position: the frame there is up (in
    /// the video sink, even while paused), and the audio after it is ready to play.
    pub fn start_landed(&self) -> bool {
        self.started && self.pending_position.is_none() && !self.unmute_on_async_done
    }

    pub fn position(&self) -> Option<Duration> {
        self.pipeline.query_position::<gst::ClockTime>().map(Duration::from)
    }

    pub fn duration(&self) -> Option<Duration> {
        self.pipeline.query_duration::<gst::ClockTime>().map(Duration::from)
    }

    /// Stop fetching and decoding video, or start again: for when nobody can see it. Audio plays
    /// on either way. On resuming, video picks up at the keyframe before the current position and
    /// catches up within a second or so.
    ///
    /// Returns whether video is now suspended. It isn't for a stream without a keyframe index
    /// (it couldn't resume in place), before the pipeline has started, or once the audio has
    /// ended: the pipeline only ends when the video has too. Nor is it when the URL only serves
    /// near real time and there's no other to switch to: resuming jumps ahead like a seek, which
    /// that URL would refuse.
    pub fn set_video_suspended(&self, suspended: bool) -> bool {
        let Some(video) = &self.video else { return false };
        let mut endpoint = lock(&video.endpoint);
        let reaches = !self.near_only || endpoint.switched || !matches!(endpoint.alternate, Alternate::Unavailable);
        let suspended =
            suspended && endpoint.resumable && reaches && self.started && !self.audio_ended.load(Ordering::Acquire);
        if suspended == (endpoint.flow == Flow::Suspended) {
            return suspended;
        }
        endpoint.flow = if suspended { Flow::Suspended } else { Flow::Resume(self.position().unwrap_or_default()) };
        set_waits_for_video(&video.sink, !suspended);
        suspended
    }

    pub fn video_suspended(&self) -> bool {
        self.video.as_ref().is_some_and(|video| lock(&video.endpoint).flow == Flow::Suspended)
    }

    /// The video and audio endpoints (just audio when playing audio alone).
    fn endpoints(&self) -> impl Iterator<Item = &Arc<Mutex<Endpoint>>> {
        self.video.as_ref().map(|video| &video.endpoint).into_iter().chain([&self.audio_endpoint])
    }

    pub fn set_alternates(&self, urls: AlternateUrls) {
        if let Some(video) = &self.video {
            lock(&video.endpoint).alternate = Alternate::Ready(urls.video);
        }
        lock(&self.audio_endpoint).alternate = Alternate::Ready(urls.audio);
    }

    pub fn alternates_failed(&self) {
        for endpoint in self.endpoints() {
            let mut endpoint = lock(endpoint);
            if matches!(endpoint.alternate, Alternate::Pending) {
                endpoint.alternate = Alternate::Unavailable;
            }
        }
    }

    /// Whether either stream switched to its alternate URL.
    pub fn using_alternates(&self) -> bool {
        self.endpoints().any(|endpoint| lock(endpoint).switched)
    }

    pub fn video_format(&self) -> Option<VideoFormat> {
        let caps = self.video.as_ref()?.sink.static_pad("sink")?.current_caps()?;
        let structure = caps.structure(0)?;
        let width = structure.get::<i32>("width").ok()?;
        let height = structure.get::<i32>("height").ok()?;
        let fraction = |name: &str| {
            structure
                .get::<gst::Fraction>(name)
                .ok()
                .filter(|value| value.numer() > 0 && value.denom() > 0)
                .map(|value| f64::from(value.numer()) / f64::from(value.denom()))
        };
        Some(VideoFormat {
            width: width.max(0) as u32,
            height: height.max(0) as u32,
            fps: fraction("framerate"),
            pixel_aspect: fraction("pixel-aspect-ratio").unwrap_or(1.0) as f32,
        })
    }

    /// The video decoder decodebin picked, e.g. `vavp9dec` (VA-API hardware).
    pub fn video_decoder(&self) -> Option<String> {
        self.pipeline.iterate_recurse().into_iter().flatten().find_map(|element| {
            let factory = element.factory()?;
            let klass = factory.metadata("klass")?;
            (klass.contains("Decoder") && klass.contains("Video")).then(|| factory.name().to_string())
        })
    }

    pub fn stats(&self) -> PlayerStats {
        let clock = self.pipeline.clock();
        let running = clock.as_ref().and_then(|clock| {
            let base = self.pipeline.base_time()?;
            Some(Duration::from_nanos(clock.time().nseconds().saturating_sub(base.nseconds())))
        });
        let (_, current, pending) = self.pipeline.state(gst::ClockTime::ZERO);
        let state = if pending == gst::State::VoidPending {
            format!("{current:?}")
        } else {
            format!("{current:?} → {pending:?}")
        };
        let video_pts = self.video.as_ref().and_then(|video| stats::last_sample_pts(&video.sink));
        let audio_pts = stats::last_sample_pts(&self.audio_sink);
        let video = self.video.as_ref();
        PlayerStats {
            clock: clock.map_or_else(|| "none".to_string(), |clock| clock.name().to_string()),
            state,
            position: self.position(),
            running,
            rate: self.rate,
            video_caps: video.and_then(|video| stats::pad_caps(&video.sink)),
            audio_caps: stats::pad_caps(&self.audio_sink),
            video_sink: video.map(|video| stats::sink_stats(&video.sink)),
            audio_sink: stats::sink_stats(&self.audio_sink),
            video_queue: video.map(|video| stats::queue_level(&video.queue)),
            audio_queue: stats::queue_level(&self.audio_queue),
            av_offset_ms: video_pts.zip(audio_pts).map(|(v, a)| (v as f64 - a as f64) / 1e6),
            video_net: video.map(|video| stats::net_stats(&video.endpoint, &video.src)),
            audio_net: stats::net_stats(&self.audio_endpoint, &self.audio_src),
            video_suspended: self.video_suspended(),
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Whatever the sources' threads are still waiting for, they needn't any more.
        for endpoint in self.endpoints() {
            lock(endpoint).closed = true;
        }
        self.pipeline.set_state(gst::State::Null).ok();
    }
}

/// The video half of the pipeline: ranged source → decodebin3 → queue → waylandsink, drawing
/// into the window's surface.
struct VideoBranch {
    sink: gst::Element,
    queue: gst::Element,
    src: gst::Element,
    endpoint: Arc<Mutex<Endpoint>>,
}

impl VideoBranch {
    fn new(
        pipeline: &gst::Pipeline,
        input: &StreamInput,
        alternates_pending: bool,
        target: &WaylandTarget,
        rect: Rect,
        http: &Http,
    ) -> Result<Self> {
        let make = |name: &str| {
            gst::ElementFactory::make(name).build().with_context(|| format!("missing GStreamer element `{name}`"))
        };
        let endpoint = Endpoint::new(input.location.clone(), alternates_pending);
        let src = http_source::add_to(pipeline, http.clone(), endpoint.clone(), input.size, input.bitrate)?;
        let decode = make("decodebin3")?;
        let queue = make("queue")?;
        let sink = make("waylandsink")?;
        pipeline.add_many([&decode, &queue, &sink])?;
        src.link(&decode)?;
        gst::Element::link_many([&queue, &sink])?;
        link_decoded_pad(&decode, &queue, "video_");

        // Share the window's Wayland connection and embed into its surface.
        sink.set_context(&target.display_context());
        let overlay = sink
            .dynamic_cast_ref::<gst_video::VideoOverlay>()
            .ok_or_else(|| anyhow!("waylandsink is not a VideoOverlay"))?;
        // SAFETY: the surface outlives the player (see `WaylandTarget::new`).
        unsafe { overlay.set_window_handle(target.surface_handle()) };
        overlay.set_render_rectangle(rect.x, rect.y, rect.w, rect.h)?;
        Ok(Self { sink, queue, src, endpoint })
    }
}

/// The pipeline's bus messages that matter to the app, as an async stream.
fn events(pipeline: &gst::Pipeline) -> Result<impl Stream<Item = PlayerEvent> + Unpin + Send + use<>> {
    let bus = pipeline.bus().ok_or_else(|| anyhow!("pipeline has no bus"))?;
    let pipeline_object = pipeline.clone().upcast::<gst::Object>();
    Ok(bus.stream().filter_map(move |message| {
        let from_pipeline = message.src() == Some(&pipeline_object);
        let event = match message.view() {
            gst::MessageView::Error(err) => {
                let source = message.src().map(|src| src.name().to_string()).unwrap_or_default();
                Some(PlayerEvent::Error(format!("{source}: {}", err.error())))
            }
            gst::MessageView::Eos(_) => Some(PlayerEvent::Eos),
            gst::MessageView::AsyncDone(_) if from_pipeline => Some(PlayerEvent::AsyncDone),
            gst::MessageView::Application(_) => {
                http_source::ready_source(&message).map(|source| PlayerEvent::SourceReady(Source(source)))
            }
            gst::MessageView::StateChanged(change) if from_pipeline => match change.current() {
                gst::State::Playing => Some(PlayerEvent::Playing),
                gst::State::Paused => Some(PlayerEvent::Paused),
                _ => None,
            },
            _ => None,
        };
        std::future::ready(event)
    }))
}

fn link_decoded_pad(decoder: &gst::Element, downstream: &gst::Element, prefix: &'static str) {
    let sink_pad = downstream.static_pad("sink").expect("queue has a sink pad");
    decoder.connect_pad_added(move |_, pad| {
        if pad.name().starts_with(prefix)
            && !sink_pad.is_linked()
            && let Err(err) = pad.link(&sink_pad)
        {
            eprintln!("moth: failed to link {prefix} pad: {err:?}");
        }
    });
}

/// Whether the pipeline waits for the video sink to preroll (show a first frame) after a seek,
/// a rate change or a pause. A suspended video branch gets no frames, so a pipeline waiting for
/// one would never play again.
fn set_waits_for_video(video_sink: &gst::Element, wait: bool) {
    video_sink.set_property("async", wait);
}

/// Keep `ended` up to date with whether the audio has played to the end, and resume suspended
/// video when it has: the pipeline ends only when every branch has, so the video branch must
/// get to its end too (from its last keyframe, which takes no time).
fn watch_audio_end(
    audio_sink: &gst::Element,
    ended: Arc<AtomicBool>,
    video: Option<(Arc<Mutex<Endpoint>>, gst::Element)>,
) {
    let Some(pad) = audio_sink.static_pad("sink") else { return };
    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::EVENT_FLUSH, move |_, info| {
        match info.event().map(|event| event.type_()) {
            Some(gst::EventType::FlushStop) => ended.store(false, Ordering::Release),
            Some(gst::EventType::Eos) => {
                ended.store(true, Ordering::Release);
                if let Some((endpoint, sink)) = &video {
                    let mut endpoint = lock(endpoint);
                    if endpoint.flow == Flow::Suspended {
                        endpoint.flow = Flow::Resume(Duration::MAX);
                        set_waits_for_video(sink, true);
                    }
                }
            }
            _ => {}
        }
        gst::PadProbeReturn::Ok
    });
}
