//! Video playback: GStreamer pipeline whose video sink (`waylandsink`) renders into a
//! `wl_subsurface` of the gpui window. Decoded frames (VA-API dmabufs) go straight to the
//! compositor; the app never touches video pixels.

use anyhow::{Context as _, Result, anyhow, bail};
use gstreamer as gst;
use gstreamer::glib::translate::from_glib_full;
use gstreamer::prelude::*;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use std::ffi::c_void;
use std::process::Command;

#[link(name = "gstwayland-1.0")]
unsafe extern "C" {
    fn gst_wl_display_handle_context_new(display: *mut c_void) -> *mut gst::ffi::GstContext;
}

/// Prefer VP9 (hardware decodable nearly everywhere), then AV1, capped at 1080p, with Opus audio.
const FORMAT: &str = "bv[height<=1080][vcodec^=vp09]+ba[acodec=opus]/bv[height<=1080][vcodec^=av01]+ba/bv[height<=1080]+ba";

pub struct StreamUrls {
    pub video: String,
    pub audio: String,
    /// e.g. "vp09.00.40.08 · 1920x1080@24 · opus"
    pub info: String,
}

/// Resolve direct stream URLs with yt-dlp. Blocking; call from a background thread.
pub fn resolve_streams(video_id: &str) -> Result<StreamUrls> {
    // Dev aid: `MOTH_FORMAT=<yt-dlp format>` overrides the format selection (e.g. to force AV1).
    let format = std::env::var("MOTH_FORMAT").unwrap_or_else(|_| FORMAT.to_string());
    let out = Command::new("yt-dlp")
        .args([
            "--no-warnings",
            "-f",
            &format,
            "--print",
            "%(vcodec)s|%(width)sx%(height)s@%(fps)s|%(acodec)s",
            "--print",
            "urls",
            "--",
            video_id,
        ])
        .output()
        .context("failed to run yt-dlp")?;
    if !out.status.success() {
        bail!("yt-dlp: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let stdout = String::from_utf8(out.stdout)?;
    let mut lines = stdout.lines().filter(|l| !l.trim().is_empty());
    let info = lines.next().ok_or_else(|| anyhow!("yt-dlp returned nothing"))?;
    let video = lines.next().ok_or_else(|| anyhow!("missing video url"))?.to_string();
    let audio = lines.next().ok_or_else(|| anyhow!("missing audio url"))?.to_string();
    let info = info.split('|').collect::<Vec<_>>().join(" · ");
    Ok(StreamUrls { video, audio, info })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

pub enum PlayerEvent {
    Error(String),
    Eos,
}

pub struct Player {
    pipeline: gst::Pipeline,
    sink: gst::Element,
    rect: Rect,
    paused: bool,
}

impl Player {
    /// Build and start the pipeline. `display`/`surface` are the gpui window's raw
    /// `wl_display`/`wl_surface`; waylandsink attaches a subsurface to `surface` at `rect`
    /// (surface-local logical coordinates).
    pub fn new(urls: &StreamUrls, display: *mut c_void, surface: *mut c_void, rect: Rect) -> Result<Self> {
        let make = |name: &str| {
            gst::ElementFactory::make(name)
                .build()
                .with_context(|| format!("missing GStreamer element `{name}`"))
        };

        let pipeline = gst::Pipeline::new();

        let vsrc = gst::ElementFactory::make("uridecodebin3")
            .property("uri", &urls.video)
            .build()?;
        let vqueue = make("queue")?;
        let sink = make("waylandsink")?;

        let asrc = gst::ElementFactory::make("uridecodebin3")
            .property("uri", &urls.audio)
            .build()?;
        let aqueue = make("queue")?;
        let aconvert = make("audioconvert")?;
        let aresample = make("audioresample")?;
        let asink = make("pipewiresink")?;

        pipeline.add_many([&vsrc, &vqueue, &sink, &asrc, &aqueue, &aconvert, &aresample, &asink])?;
        gst::Element::link_many([&vqueue, &sink])?;
        gst::Element::link_many([&aqueue, &aconvert, &aresample, &asink])?;
        link_decoded_pad(&vsrc, &vqueue, "video_");
        link_decoded_pad(&asrc, &aqueue, "audio_");

        // Share gpui's Wayland connection and embed into its surface.
        if display.is_null() || surface.is_null() {
            bail!("no Wayland surface available");
        }
        let context: gst::Context = unsafe { from_glib_full(gst_wl_display_handle_context_new(display)) };
        sink.set_context(&context);
        let overlay = sink
            .dynamic_cast_ref::<gst_video::VideoOverlay>()
            .ok_or_else(|| anyhow!("waylandsink is not a VideoOverlay"))?;
        unsafe { overlay.set_window_handle(surface as usize) };
        overlay.set_render_rectangle(rect.x, rect.y, rect.w, rect.h)?;

        pipeline
            .set_state(gst::State::Playing)
            .context("failed to start pipeline")?;

        Ok(Self { pipeline, sink, rect, paused: false })
    }

    pub fn set_rect(&mut self, rect: Rect) {
        if rect != self.rect {
            self.rect = rect;
            if let Some(overlay) = self.sink.dynamic_cast_ref::<gst_video::VideoOverlay>() {
                let _ = overlay.set_render_rectangle(rect.x, rect.y, rect.w, rect.h);
                overlay.expose();
            }
        }
    }

    pub fn is_playing(&self) -> bool {
        self.pipeline.current_state() == gst::State::Playing
    }

    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        let target = if self.paused { gst::State::Paused } else { gst::State::Playing };
        let _ = self.pipeline.set_state(target);
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// (position, duration) in seconds.
    pub fn position_duration(&self) -> (Option<u64>, Option<u64>) {
        let pos = self.pipeline.query_position::<gst::ClockTime>().map(|t| t.seconds());
        let dur = self.pipeline.query_duration::<gst::ClockTime>().map(|t| t.seconds());
        (pos, dur)
    }

    pub fn seek_seconds(&self, seconds: u64) {
        let _ = self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_seconds(seconds),
        );
    }

    pub fn drain_events(&self) -> Vec<PlayerEvent> {
        let mut events = Vec::new();
        let Some(bus) = self.pipeline.bus() else { return events };
        while let Some(msg) = bus.pop() {
            match msg.view() {
                gst::MessageView::Error(err) => {
                    let src = msg.src().map(|s| s.name().to_string()).unwrap_or_default();
                    events.push(PlayerEvent::Error(format!("{src}: {}", err.error())));
                }
                gst::MessageView::Eos(_) => events.push(PlayerEvent::Eos),
                _ => {}
            }
        }
        events
    }

    /// Name of the video decoder that decodebin picked (e.g. `vavp9dec` = VA-API hardware).
    pub fn video_decoder(&self) -> Option<String> {
        self.pipeline.iterate_recurse().into_iter().flatten().find_map(|el| {
            let factory = el.factory()?;
            let klass = factory.metadata("klass")?;
            (klass.contains("Decoder") && klass.contains("Video")).then(|| factory.name().to_string())
        })
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn link_decoded_pad(src: &gst::Element, downstream: &gst::Element, prefix: &'static str) {
    let sink_pad = downstream.static_pad("sink").expect("queue has a sink pad");
    src.connect_pad_added(move |_, pad| {
        if pad.name().starts_with(prefix) && !sink_pad.is_linked() {
            if let Err(err) = pad.link(&sink_pad) {
                eprintln!("failed to link {prefix} pad: {err:?}");
            }
        }
    });
}
