//! Video playback: GStreamer pipeline whose video sink (`waylandsink`) renders into a
//! `wl_subsurface` of the gpui window. Decoded frames (VA-API dmabufs) go straight to the
//! compositor; the app never touches video pixels.

use anyhow::{Context as _, Result, anyhow, bail};
use gstreamer as gst;
use gstreamer::glib::translate::from_glib_full;
use gstreamer::prelude::*;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use gstreamer_app as gst_app;
use std::ffi::c_void;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[link(name = "gstwayland-1.0")]
unsafe extern "C" {
    fn gst_wl_display_handle_context_new(display: *mut c_void) -> *mut gst::ffi::GstContext;
}

/// Direct (non-HLS) streams only: VP9, then AV1, then anything, up to `max_height`; Opus audio
/// preferred. yt-dlp reports VP9 as `vp9` for direct streams and `vp09` for HLS ones.
fn default_format(max_height: u32) -> String {
    let h = max_height;
    format!(
        "bv[height<={h}][vcodec^=vp9][protocol=https]/bv[height<={h}][vcodec^=av01][protocol=https]/bv[height<={h}][protocol=https]\
+ba[acodec=opus][protocol=https]/ba[protocol=https]"
    )
}

pub struct StreamUrls {
    pub video: String,
    pub audio: String,
    /// Content lengths in bytes, when the resolver knows them.
    pub video_size: Option<u64>,
    pub audio_size: Option<u64>,
    /// Average bitrates in bits per second, when known; used to size range requests.
    pub video_bitrate: Option<u32>,
    pub audio_bitrate: Option<u32>,
    /// YouTube format ids, used to fetch the same files through another client.
    pub video_itag: Option<u32>,
    pub audio_itag: Option<u32>,
    /// Video frame size as reported by the resolver, used to shape the player before decoding.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// e.g. "vp09.00.40.08 · 1920x1080@24 · opus"
    pub info: String,
}

/// Network access for the stream sources: an HTTP client plus the tokio runtime it lives on.
#[derive(Clone)]
pub struct Http {
    pub client: reqwest::Client,
    pub runtime: tokio::runtime::Handle,
}

/// Resolve direct stream URLs with yt-dlp. Blocking; call from a background thread.
/// `format` defaults to the best streams up to `max_height`; pass `"<video itag>+<audio itag>"`
/// to fetch specific files.
pub fn resolve_streams(video_id: &str, format: Option<String>, max_height: u32) -> Result<StreamUrls> {
    // Dev aid: `MOTH_FORMAT=<yt-dlp format>` overrides the default selection (e.g. to force AV1).
    let format = format
        .or_else(|| std::env::var("MOTH_FORMAT").ok())
        .unwrap_or_else(|| default_format(max_height));
    // yt-dlp is a fallback that may run alongside playback, so keep it out of the way.
    let out = Command::new("nice")
        .args([
            "-n",
            "10",
            "yt-dlp",
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
    // info is "vcodec|WIDTHxHEIGHT@fps|acodec".
    let (width, height) = info
        .split('|')
        .nth(1)
        .and_then(|size| size.split('@').next()?.split_once('x'))
        .map(|(w, h)| (w.parse().ok(), h.parse().ok()))
        .unwrap_or((None, None));
    let info = info.split('|').collect::<Vec<_>>().join(" · ");
    Ok(StreamUrls {
        video,
        audio,
        video_size: None,
        audio_size: None,
        video_bitrate: None,
        audio_bitrate: None,
        video_itag: None,
        audio_itag: None,
        width,
        height,
        info,
    })
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

/// Where a (re)built pipeline should begin: used when switching streams mid-video.
#[derive(Clone, Copy, Debug)]
pub struct Start {
    pub position: u64,
    pub rate: f64,
    pub paused: bool,
}

pub struct Player {
    pipeline: gst::Pipeline,
    sink: gst::Element,
    rect: Rect,
    paused: bool,
    rate: f64,
    /// Applied once the pipeline has prerolled.
    pending_start: Option<Start>,
    video_source: Arc<Mutex<Source>>,
    audio_source: Arc<Mutex<Source>>,
}

impl Player {
    /// Build and start the pipeline. `display`/`surface` are the gpui window's raw
    /// `wl_display`/`wl_surface`; waylandsink attaches a subsurface to `surface` at `rect`
    /// (surface-local logical coordinates).
    /// `fallback_pending` means alternate URLs for the same files are being resolved and will be
    /// delivered through [`Player::set_fallback`] (or [`Player::fallback_failed`]).
    pub fn new(
        urls: &StreamUrls,
        start: Option<Start>,
        fallback_pending: bool,
        http: &Http,
        display: *mut c_void,
        surface: *mut c_void,
        rect: Rect,
    ) -> Result<Self> {
        let video_source = Source::new(urls.video.clone(), fallback_pending);
        let audio_source = Source::new(urls.audio.clone(), fallback_pending);
        let make = |name: &str| {
            gst::ElementFactory::make(name)
                .build()
                .with_context(|| format!("missing GStreamer element `{name}`"))
        };

        let pipeline = gst::Pipeline::new();

        let vsrc = range_source(http.clone(), video_source.clone(), urls.video_size, urls.video_bitrate)?;
        let vdecode = make("decodebin3")?;
        let vqueue = make("queue")?;
        let sink = make("waylandsink")?;

        let asrc = range_source(http.clone(), audio_source.clone(), urls.audio_size, urls.audio_bitrate)?;
        let adecode = make("decodebin3")?;
        let aqueue = make("queue")?;
        let aconvert = make("audioconvert")?;
        // Keeps the pitch natural when playing faster or slower.
        let tempo = make("scaletempo")?;
        let aconvert_out = make("audioconvert")?;
        let aresample = make("audioresample")?;
        let asink = make("pipewiresink")?;

        pipeline.add_many([
            &vsrc, &vdecode, &vqueue, &sink, &asrc, &adecode, &aqueue, &aconvert, &tempo, &aconvert_out,
            &aresample, &asink,
        ])?;
        vsrc.link(&vdecode)?;
        asrc.link(&adecode)?;
        gst::Element::link_many([&vqueue, &sink])?;
        gst::Element::link_many([&aqueue, &aconvert, &tempo, &aconvert_out, &aresample, &asink])?;
        link_decoded_pad(&vdecode, &vqueue, "video_");
        link_decoded_pad(&adecode, &aqueue, "audio_");

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
            // With a start position, preroll paused first so the seek lands before anything plays.
            .set_state(if start.is_some() { gst::State::Paused } else { gst::State::Playing })
            .context("failed to start pipeline")?;

        Ok(Self {
            pipeline,
            sink,
            rect,
            paused: start.is_some_and(|start| start.paused),
            rate: start.map_or(1.0, |start| start.rate),
            pending_start: start,
            video_source,
            audio_source,
        })
    }

    /// Call regularly: applies the start position once the pipeline has prerolled.
    pub fn tick(&mut self) {
        let Some(start) = self.pending_start else { return };
        let (_, current, pending) = self.pipeline.state(gst::ClockTime::ZERO);
        if current < gst::State::Paused || pending != gst::State::VoidPending {
            return;
        }
        self.pending_start = None;
        self.seek_to(start.position);
        if !start.paused {
            self.pipeline.set_state(gst::State::Playing).ok();
        }
    }

    /// Change the playback speed, keeping the current position.
    pub fn set_rate(&mut self, rate: f64) {
        if let Some(start) = self.pending_start.as_mut() {
            start.rate = rate;
            self.rate = rate;
            return;
        }
        self.rate = rate;
        if let Some(position) = self.pipeline.query_position::<gst::ClockTime>() {
            self.seek_clock(position);
        }
    }

    fn seek_to(&self, seconds: u64) {
        self.seek_clock(gst::ClockTime::from_seconds(seconds));
    }

    fn seek_clock(&self, position: gst::ClockTime) {
        let result = self.pipeline.seek(
            self.rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            Some(position),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
        if let Err(err) = result {
            eprintln!("moth: seek failed: {err}");
        }
    }

    pub fn set_fallback(&self, video: String, audio: String) {
        lock(&self.video_source).fallback = Fallback::Ready(video);
        lock(&self.audio_source).fallback = Fallback::Ready(audio);
    }

    pub fn fallback_failed(&self) {
        for source in [&self.video_source, &self.audio_source] {
            let mut source = lock(source);
            if matches!(source.fallback, Fallback::Pending) {
                source.fallback = Fallback::Unavailable;
            }
        }
    }

    /// Whether either stream had to switch to its fallback URL.
    pub fn using_fallback(&self) -> bool {
        lock(&self.video_source).switched || lock(&self.audio_source).switched
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
        self.seek_to(seconds);
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

    /// Display aspect ratio (width / height) of the decoded video, once caps are negotiated.
    pub fn display_aspect(&self) -> Option<f32> {
        let caps = self.sink.static_pad("sink")?.current_caps()?;
        let structure = caps.structure(0)?;
        let width = structure.get::<i32>("width").ok()?;
        let height = structure.get::<i32>("height").ok()?;
        let par = structure
            .get::<gst::Fraction>("pixel-aspect-ratio")
            .map(|par| par.numer() as f32 / par.denom() as f32)
            .unwrap_or(1.0);
        (width > 0 && height > 0).then(|| width as f32 * par / height as f32)
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

/// YouTube's CDN rejects requests without a bounded `Range` (even `bytes=0-` gets a 403), so
/// GStreamer's own HTTP source can't play these URLs; this feeds an `appsrc` with bounded ranged
/// requests instead, much like yt-dlp downloads.
///
/// URLs from YouTube's iOS client (what RustyPipe can use without a PO token) additionally only
/// serve bytes a little ahead of real-time playback, so a seek forward gets a 403. Web-client
/// URLs for the same format are byte-identical and unrestricted, so on a 403 the source switches
/// to that fallback URL (resolved in the background) and carries on.
const MIN_CHUNK_BYTES: u64 = 64 << 10;
const MAX_CHUNK_BYTES: u64 = 1 << 20;
const DEFAULT_CHUNK_BYTES: u64 = 512 << 10;
const CHUNK_SECONDS: u64 = 10;
const FALLBACK_WAIT: Duration = Duration::from_secs(20);

enum Fallback {
    Unavailable,
    Pending,
    Ready(String),
}

struct Source {
    url: String,
    fallback: Fallback,
    switched: bool,
}

impl Source {
    fn new(url: String, fallback_pending: bool) -> Arc<Mutex<Self>> {
        let fallback = if fallback_pending { Fallback::Pending } else { Fallback::Unavailable };
        Arc::new(Mutex::new(Self { url, fallback, switched: false }))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct RangeState {
    position: u64,
    /// Bumped on every seek so a fetch that was in flight during the seek is discarded.
    epoch: u64,
    eos: bool,
}

/// Roughly `CHUNK_SECONDS` of media when the bitrate (bits per second) is known.
fn chunk_size(bitrate: Option<u32>) -> u64 {
    match bitrate {
        Some(bitrate) if bitrate > 0 => (u64::from(bitrate) / 8 * CHUNK_SECONDS).clamp(MIN_CHUNK_BYTES, MAX_CHUNK_BYTES),
        _ => DEFAULT_CHUNK_BYTES,
    }
}

enum Fetched {
    Data(Vec<u8>),
    PastEnd,
    Forbidden,
}

fn range_source(http: Http, source: Arc<Mutex<Source>>, size: Option<u64>, bitrate: Option<u32>) -> Result<gst::Element> {
    let chunk = chunk_size(bitrate);
    let appsrc = gst_app::AppSrc::builder()
        .stream_type(gst_app::AppStreamType::Seekable)
        .format(gst::Format::Bytes)
        .size(size.map_or(-1, |size| size as i64))
        .max_bytes(2 * chunk)
        .build();
    let state = Arc::new(Mutex::new(RangeState { position: 0, epoch: 0, eos: false }));

    appsrc.set_callbacks(
        gst_app::AppSrcCallbacks::builder()
            .need_data({
                let state = state.clone();
                move |appsrc, _| {
                    if let Err(err) = feed(appsrc, &state, &source, &http, size, chunk) {
                        eprintln!("moth: stream fetch failed: {err:#}");
                        appsrc
                            .post_message(
                                gst::message::Error::builder(gst::ResourceError::Read, &format!("{err:#}"))
                                    .src(appsrc)
                                    .build(),
                            )
                            .ok();
                    }
                }
            })
            .seek_data({
                let state = state.clone();
                move |_, offset| {
                    let mut state = lock(&state);
                    state.position = offset;
                    state.epoch += 1;
                    state.eos = false;
                    true
                }
            })
            .build(),
    );
    Ok(appsrc.upcast())
}

/// Fetch and push the next chunk. Runs on appsrc's own streaming thread, so blocking is fine.
fn feed(
    appsrc: &gst_app::AppSrc,
    state: &Mutex<RangeState>,
    source: &Mutex<Source>,
    http: &Http,
    size: Option<u64>,
    chunk: u64,
) -> Result<()> {
    let (start, epoch) = {
        let state = lock(state);
        if state.eos {
            return Ok(());
        }
        (state.position, state.epoch)
    };
    if size.is_some_and(|size| start >= size) {
        mark_eos(state, epoch, appsrc);
        return Ok(());
    }
    let mut end = start + chunk - 1;
    if let Some(size) = size {
        end = end.min(size - 1);
    }

    let waiting_since = std::time::Instant::now();
    let bytes = loop {
        let url = lock(source).url.clone();
        match http.runtime.block_on(fetch_range(&http.client, &url, start, end))? {
            Fetched::Data(bytes) => break bytes,
            Fetched::PastEnd => {
                mark_eos(state, epoch, appsrc);
                return Ok(());
            }
            Fetched::Forbidden => {
                let mut source = lock(source);
                match std::mem::replace(&mut source.fallback, Fallback::Unavailable) {
                    Fallback::Ready(fallback) => {
                        source.url = fallback;
                        source.switched = true;
                    }
                    Fallback::Pending if waiting_since.elapsed() < FALLBACK_WAIT => {
                        source.fallback = Fallback::Pending;
                        drop(source);
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    Fallback::Pending | Fallback::Unavailable => {
                        bail!("YouTube refused bytes {start}-{end} (HTTP 403)")
                    }
                }
            }
        }
        if lock(state).epoch != epoch {
            return Ok(());
        }
    };

    let length = bytes.len() as u64;
    {
        let mut state = lock(state);
        if state.epoch != epoch {
            return Ok(());
        }
        state.position = start + length;
        state.eos = length < end - start + 1;
    }
    let mut buffer = gst::Buffer::from_slice(bytes);
    if let Some(buffer) = buffer.get_mut() {
        buffer.set_offset(start);
        buffer.set_offset_end(start + length);
    }
    // Push fails only when flushing or shutting down; the next need-data picks up from there.
    if appsrc.push_buffer(buffer).is_ok() && {
        let state = lock(state);
        state.eos && state.epoch == epoch
    } {
        appsrc.end_of_stream().ok();
    }
    Ok(())
}

fn mark_eos(state: &Mutex<RangeState>, epoch: u64, appsrc: &gst_app::AppSrc) {
    let current = state.lock().map(|mut state| {
        if state.epoch == epoch {
            state.eos = true;
        }
        state.epoch
    });
    if current.is_ok_and(|current| current == epoch) {
        // Errors here only mean the pipeline is already shutting down.
        appsrc.end_of_stream().ok();
    }
}

async fn fetch_range(client: &reqwest::Client, url: &str, start: u64, end: u64) -> Result<Fetched> {
    let response = client
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
        .timeout(Duration::from_secs(20))
        .send()
        .await?;
    match response.status() {
        reqwest::StatusCode::RANGE_NOT_SATISFIABLE => Ok(Fetched::PastEnd),
        reqwest::StatusCode::FORBIDDEN => Ok(Fetched::Forbidden),
        _ => Ok(Fetched::Data(response.error_for_status()?.bytes().await?.to_vec())),
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
