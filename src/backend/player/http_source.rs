//! An `appsrc` that fetches a stream over HTTP in bounded byte ranges.
//!
//! Why not GStreamer's own HTTP source: YouTube's CDN rejects requests without a bounded `Range`
//! (even `bytes=0-` gets a 403), so this feeds the pipeline with bounded ranged requests, much
//! like yt-dlp downloads.
//!
//! Some servers stop serving part-way (YouTube's iOS-client URLs only serve a little ahead of
//! real-time playback, so a long seek gets a 403). If the stream has an alternate URL for the same
//! bytes, a refused request switches to it and carries on; while one is still being resolved, the
//! request waits for it a while. With no other URL to turn to, it tries again a few times before
//! playback fails, since servers also refuse for reasons that pass.
//!
//! Seeking is normally the demuxer's job: it works out which bytes it wants next and the source
//! just serves them. Fragmented MP4 is the exception. `qtdemux` refuses to seek in one that comes
//! as a byte stream, and expects its source to do it, as a DASH client would (see
//! [`super::mp4_index`]). So for those the source works in time instead of bytes: asked for a
//! position, it carries on from the fragment the position is in. Which of the two a stream needs
//! shows in its first bytes, and an `appsrc` can't change its mind once started, so the source
//! fetches those before it starts.
//!
//! A source can also be suspended ([`Flow`]): it stops fetching, so its branch of the pipeline
//! runs dry and stops decoding, and when resumed it carries on from the keyframe before the
//! position it's told, without a seek. The player uses that to drop the video of a window
//! nobody can see while the audio plays on. It needs a keyframe index: the fragment index of a
//! fragmented MP4, or the cues of a WebM.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::stats::NetStats;
use super::{mp4_index, webm_index};
use crate::backend::Http;
use crate::backend::media::Location;
use crate::sync::lock;

const MIN_CHUNK_BYTES: u64 = 64 << 10;
const MAX_CHUNK_BYTES: u64 = 1 << 20;
const DEFAULT_CHUNK_BYTES: u64 = 512 << 10;
/// Each request asks for about this much media, when the bitrate is known.
const CHUNK_DURATION: Duration = Duration::from_secs(10);
/// How far into a file to look for a segment index. The index of a ten-hour video is about
/// 100 KiB, and it sits right after the (small) movie header.
const MAX_HEADER_BYTES: u64 = 4 << 20;
/// How long a refused request waits for an alternate URL that's still being resolved. That can
/// take a while (for YouTube: a yt-dlp run, then however long YouTube holds the URLs back).
const ALTERNATE_WAIT: Duration = Duration::from_secs(45);
/// A refused request with no other URL to turn to is tried again this often, this far apart.
const REFUSED_RETRIES: u32 = 3;
const REFUSED_RETRY_PAUSE: Duration = Duration::from_secs(1);
/// How often a waiting request checks whether it's still wanted.
const WAIT_STEP: Duration = Duration::from_millis(100);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Name of the bus message a source posts once it can be started.
const READY_MESSAGE: &str = "moth-source-ready";

pub enum Alternate {
    Unavailable,
    Pending,
    Ready(Location),
}

/// Where a stream is fetched from, shared between the streaming thread and the player.
pub struct Endpoint {
    pub location: Location,
    pub alternate: Alternate,
    /// Whether the alternate URL replaced the original.
    pub switched: bool,
    pub net: NetStats,
    /// The player is gone: nobody is waiting for this stream any more.
    pub closed: bool,
    pub flow: Flow,
    /// The source found a keyframe index, so it can be suspended and resumed in place.
    pub resumable: bool,
}

/// Whether a source fetches.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Flow {
    #[default]
    Running,
    /// Fetch nothing: what's buffered plays out, then the branch goes idle.
    Suspended,
    /// Carry on from the keyframe at or before this position (`Duration::MAX`: the last one).
    Resume(Duration),
}

impl Endpoint {
    pub fn new(location: Location, alternate_pending: bool) -> Arc<Mutex<Self>> {
        let alternate = if alternate_pending { Alternate::Pending } else { Alternate::Unavailable };
        Arc::new(Mutex::new(Self {
            location,
            alternate,
            switched: false,
            net: NetStats::default(),
            closed: false,
            flow: Flow::Running,
            resumable: false,
        }))
    }
}

struct RangeState {
    position: u64,
    /// Bumped on every seek, so a fetch that was in flight during the seek is discarded.
    epoch: u64,
    eos: bool,
    /// Whether anything has been pushed yet. Until then the demuxer has nothing, so wherever
    /// it's asked to start, the source starts with the beginning of the file.
    started: bool,
}

/// Roughly `CHUNK_DURATION` of media when the bitrate (bits per second) is known.
fn chunk_size(bitrate: Option<u32>) -> u64 {
    match bitrate {
        Some(bitrate) if bitrate > 0 => {
            (u64::from(bitrate) / 8 * CHUNK_DURATION.as_secs()).clamp(MIN_CHUNK_BYTES, MAX_CHUNK_BYTES)
        }
        _ => DEFAULT_CHUNK_BYTES,
    }
}

enum Fetched {
    Data(Vec<u8>),
    PastEnd,
    Forbidden,
}

/// What a refused request can do next.
enum Refused {
    /// The endpoint now has another URL to try.
    Switched,
    NoAlternate,
    Abandoned,
}

/// What became of a request for a byte range.
enum Range {
    Data(Vec<u8>),
    PastEnd,
    /// Nobody wants it any more (a seek, or the player shutting down).
    Abandoned,
}

/// Add a source for `endpoint`'s stream to `pipeline`. It takes no part in the pipeline's state
/// changes until it has seen the beginning of the stream: then it posts a message on the bus (see
/// [`ready_source`]), and wants to be [`start`]ed.
pub fn add_to(
    pipeline: &gst::Pipeline,
    http: Http,
    endpoint: Arc<Mutex<Endpoint>>,
    size: Option<u64>,
    bitrate: Option<u32>,
) -> Result<gst::Element> {
    let chunk = chunk_size(bitrate);
    let appsrc = gst_app::AppSrc::builder().stream_type(gst_app::AppStreamType::Seekable).max_bytes(2 * chunk).build();
    // Not to be started before `open` knows which format it works in.
    appsrc.set_locked_state(true);
    pipeline.add(&appsrc)?;
    let fetcher = Fetcher { http, endpoint, size, chunk };
    // A thread of its own rather than the tokio runtime's: it blocks on the network, and nothing
    // should have to wait for it when moth quits.
    std::thread::Builder::new()
        .name("moth-source".into())
        .spawn({
            let appsrc = appsrc.clone();
            move || {
                if let Err(err) = open(fetcher, &appsrc) {
                    post_error(&appsrc, &err);
                }
            }
        })
        .context("failed to start a source thread")?;
    Ok(appsrc.upcast())
}

fn post_error(appsrc: &gst_app::AppSrc, err: &anyhow::Error) {
    eprintln!("moth: stream fetch failed: {err:#}");
    let message = gst::message::Error::builder(gst::ResourceError::Read, &format!("{err:#}")).src(appsrc).build();
    appsrc.post_message(message).ok();
}

/// Fetch the beginning of the stream, set the source up for what it turns out to be, and
/// announce that it's ready.
fn open(fetcher: Fetcher, appsrc: &gst_app::AppSrc) -> Result<()> {
    let mut prefix = Vec::new();
    let mut wanted = fetcher.chunk;
    let index = loop {
        let start = prefix.len() as u64;
        let end = fetcher.size.map_or(wanted, |size| wanted.min(size)).max(start + 1) - 1;
        match fetcher.fetch(start, end, || fetcher.closed())? {
            Range::Data(bytes) => prefix.extend(bytes),
            Range::PastEnd => break None,
            Range::Abandoned => return Ok(()),
        }
        let complete = (prefix.len() as u64) <= end || fetcher.size.is_some_and(|size| prefix.len() as u64 >= size);
        match Index::probe(&prefix) {
            Probe::Indexed(index) => break Some(index),
            Probe::NeedsBytes(bytes) if !complete && bytes <= MAX_HEADER_BYTES => wanted = bytes.max(wanted),
            Probe::NeedsBytes(_) | Probe::Unindexed => break None,
        }
    };
    lock(&fetcher.endpoint).resumable = index.is_some();

    match &index {
        Some(Index::Mp4(index)) => {
            appsrc.set_format(gst::Format::Time);
            appsrc.set_duration(gst::ClockTime::from_nseconds(index.duration.as_nanos() as u64));
            // With a source that works in time, qtdemux passes the media's own timestamps on
            // instead of applying the track's edit list, which would leave this stream late by
            // what the edit list skips (a frame, for H.264). Shifting the source's output
            // makes up for it.
            if let Some(pad) = appsrc.static_pad("src") {
                pad.set_offset(-(index.presentation_offset.as_nanos() as i64));
            }
        }
        Some(Index::WebM(_)) | None => {
            appsrc.set_format(gst::Format::Bytes);
            appsrc.set_size(fetcher.size.map_or(-1, |size| size as i64));
        }
    }

    let state = Arc::new(Mutex::new(RangeState { position: 0, epoch: 0, eos: false, started: false }));
    let feeder = Feeder { fetcher, prefix, state: state.clone(), index: index.clone() };
    appsrc.set_callbacks(
        gst_app::AppSrcCallbacks::builder()
            .need_data(move |appsrc, _| {
                if let Err(err) = feeder.feed(appsrc) {
                    post_error(appsrc, &err);
                }
            })
            .seek_data(move |_, offset| {
                let mut state = lock(&state);
                state.position = match &index {
                    // `offset` is a time in nanoseconds.
                    Some(Index::Mp4(index)) if state.started => index.fragment_offset(Duration::from_nanos(offset)),
                    Some(Index::Mp4(_)) => 0,
                    Some(Index::WebM(_)) | None => offset,
                };
                state.epoch += 1;
                state.eos = false;
                true
            })
            .build(),
    );

    // The player starts it (`start`), on the thread that changes the pipeline's state. Started
    // from this thread, it could get ahead of the elements it feeds while the pipeline is still
    // starting those, or be left running by a pipeline that has just shut down.
    let ready = gst::message::Application::builder(gst::Structure::new_empty(READY_MESSAGE)).src(appsrc).build();
    appsrc.post_message(ready).context("failed to announce the stream source")
}

/// The source that `message` says is ready to [`start`], if that's what the message is.
pub fn ready_source(message: &gst::Message) -> Option<gst::Element> {
    let gst::MessageView::Application(application) = message.view() else { return None };
    let is_ready = application.structure().is_some_and(|structure| structure.name() == READY_MESSAGE);
    message.src().filter(|_| is_ready)?.clone().downcast().ok()
}

/// Start a source that has announced it's ready. Call from the thread that changes the
/// pipeline's state.
pub fn start(source: &gst::Element) -> Result<()> {
    source.set_locked_state(false);
    source.sync_state_with_parent().context("failed to start a stream source")
}

struct Fetcher {
    http: Http,
    endpoint: Arc<Mutex<Endpoint>>,
    size: Option<u64>,
    chunk: u64,
}

impl Fetcher {
    fn closed(&self) -> bool {
        lock(&self.endpoint).closed
    }

    /// Fetch bytes `start..=end` (fewer at the end of the file), switching to the alternate URL
    /// if the server refuses. Blocking; `abandoned` is checked while waiting, to give up early.
    fn fetch(&self, start: u64, end: u64, abandoned: impl Fn() -> bool) -> Result<Range> {
        let mut retries = 0;
        loop {
            let location = lock(&self.endpoint).location.clone();
            let requested_at = Instant::now();
            let fetched = self.http.runtime.block_on(fetch_range(&self.http.client, &location, start, end))?;
            self.record(&fetched, requested_at.elapsed());
            match fetched {
                Fetched::Data(bytes) => return Ok(Range::Data(bytes)),
                Fetched::PastEnd => return Ok(Range::PastEnd),
                Fetched::Forbidden => {}
            }
            match self.switch_to_alternate(start, &abandoned) {
                Refused::Switched => retries = 0,
                Refused::NoAlternate if retries < REFUSED_RETRIES => {
                    if retries == 0 {
                        eprintln!("moth: refused at byte {start}, with no other URL to try; trying again");
                    }
                    retries += 1;
                    if !pause(REFUSED_RETRY_PAUSE, &abandoned) {
                        return Ok(Range::Abandoned);
                    }
                }
                Refused::NoAlternate => {
                    bail!("the server refused bytes {start}-{end} of {} (HTTP 403)", describe(&location.url))
                }
                Refused::Abandoned => return Ok(Range::Abandoned),
            }
        }
    }

    fn record(&self, fetched: &Fetched, elapsed: Duration) {
        let net = &mut lock(&self.endpoint).net;
        net.requests += 1;
        net.last_request = elapsed;
        match fetched {
            Fetched::Data(bytes) => {
                net.bytes += bytes.len() as u64;
                net.last_rate = bytes.len() as f64 / elapsed.as_secs_f64().max(1e-6);
            }
            Fetched::Forbidden => net.forbidden += 1,
            Fetched::PastEnd => {}
        }
    }

    /// After a refused request: switch to the alternate URL, waiting for it while it's still
    /// being resolved. Waiting, not asking the server again: it would only refuse again.
    fn switch_to_alternate(&self, start: u64, abandoned: &impl Fn() -> bool) -> Refused {
        let waiting_since = Instant::now();
        loop {
            {
                let mut endpoint = lock(&self.endpoint);
                match std::mem::replace(&mut endpoint.alternate, Alternate::Unavailable) {
                    Alternate::Ready(location) => {
                        eprintln!(
                            "moth: refused at byte {start}; switching to the alternate URL ({}, {} headers)",
                            describe(&location.url),
                            location.headers.len()
                        );
                        endpoint.location = location;
                        endpoint.switched = true;
                        return Refused::Switched;
                    }
                    Alternate::Pending if waiting_since.elapsed() < ALTERNATE_WAIT => {
                        endpoint.alternate = Alternate::Pending;
                    }
                    Alternate::Pending | Alternate::Unavailable => return Refused::NoAlternate,
                }
            }
            if !pause(WAIT_STEP, abandoned) {
                return Refused::Abandoned;
            }
        }
    }
}

/// Sleep for `duration`, unless `abandoned` cuts it short: then return false.
fn pause(duration: Duration, abandoned: &impl Fn() -> bool) -> bool {
    let until = Instant::now() + duration;
    loop {
        if abandoned() {
            return false;
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(WAIT_STEP));
    }
}

/// Whether the pipeline wants `appsrc` to stop what it's doing: it's flushing the source for a
/// seek, or shutting it down. Both wait, on the UI thread, for the streaming thread to return
/// from `need-data`. So that must not sit out a wait, least of all the one for an alternate URL,
/// which the UI thread delivers.
fn interrupted(appsrc: &gst_app::AppSrc) -> bool {
    appsrc.static_pad("src").is_some_and(|pad| pad.pad_flags().contains(gst::PadFlags::FLUSHING))
}

/// Feeds the started `appsrc`. Runs on its streaming thread, so blocking on the network is fine.
struct Feeder {
    fetcher: Fetcher,
    /// The beginning of the file, which `open` fetched: served from here rather than fetched
    /// again.
    prefix: Vec<u8>,
    state: Arc<Mutex<RangeState>>,
    index: Option<Index>,
}

impl Feeder {
    /// Wait while the source is suspended. `None` if the wait was cut short (a flush, or the
    /// player shutting down); otherwise whether the source just resumed, at a new position.
    fn wait_while_suspended(&self, appsrc: &gst_app::AppSrc) -> Option<bool> {
        loop {
            {
                let mut endpoint = lock(&self.fetcher.endpoint);
                match endpoint.flow {
                    Flow::Running => return Some(false),
                    Flow::Resume(position) => {
                        endpoint.flow = Flow::Running;
                        let Some(index) = &self.index else { return Some(false) };
                        let mut state = lock(&self.state);
                        state.position = index.keyframe_offset(position);
                        state.eos = false;
                        return Some(true);
                    }
                    Flow::Suspended => {}
                }
            }
            if !pause(WAIT_STEP, &|| interrupted(appsrc) || self.fetcher.closed()) {
                return None;
            }
        }
    }

    /// Fetch and push the next chunk.
    fn feed(&self, appsrc: &gst_app::AppSrc) -> Result<()> {
        let state = &*self.state;
        let Some(resumed) = self.wait_while_suspended(appsrc) else { return Ok(()) };
        let (start, epoch) = {
            let state = lock(state);
            if state.eos {
                return Ok(());
            }
            (state.position, state.epoch)
        };
        let size = self.fetcher.size;
        if size.is_some_and(|size| start >= size) {
            mark_eos(state, epoch, appsrc);
            return Ok(());
        }
        let mut end = start + self.fetcher.chunk - 1;
        if let Some(size) = size {
            end = end.min(size - 1);
        }

        let cached =
            usize::try_from(start).ok().and_then(|start| self.prefix.get(start..)).filter(|rest| !rest.is_empty());
        let bytes = match cached {
            Some(rest) => {
                // Only as far as the prefix goes; the next chunk carries on from there.
                end = end.min(self.prefix.len() as u64 - 1);
                rest[..(end - start + 1) as usize].to_vec()
            }
            None => match self.fetcher.fetch(start, end, || interrupted(appsrc) || self.fetcher.closed())? {
                Range::Data(bytes) => bytes,
                Range::PastEnd => {
                    mark_eos(state, epoch, appsrc);
                    return Ok(());
                }
                Range::Abandoned => return Ok(()),
            },
        };

        let length = bytes.len() as u64;
        {
            let mut state = lock(state);
            if state.epoch != epoch {
                return Ok(());
            }
            state.position = start + length;
            state.eos = length < end - start + 1;
            state.started = true;
        }
        let mut buffer = gst::Buffer::from_slice(bytes);
        if let Some(buffer) = buffer.get_mut() {
            // The demuxer has to drop what it was in the middle of and look for the next
            // fragment (cluster) header.
            if resumed {
                buffer.set_flags(gst::BufferFlags::DISCONT);
            }
            buffer.set_offset(start);
            buffer.set_offset_end(start + length);
        }
        // Pushing fails only when flushing or shutting down; the next need-data carries on.
        if appsrc.push_buffer(buffer).is_ok() && {
            let state = lock(state);
            state.eos && state.epoch == epoch
        } {
            appsrc.end_of_stream().ok();
        }
        Ok(())
    }
}

/// Where a stream's keyframes are, if its first bytes say.
#[derive(Clone, Debug)]
enum Index {
    /// Fragmented MP4. The source works in time for these, and seeks with the index too.
    Mp4(mp4_index::SegmentIndex),
    /// WebM with its cues up front. `matroskademux` seeks by itself; this is for resuming.
    WebM(webm_index::CueIndex),
}

enum Probe {
    Unindexed,
    NeedsBytes(u64),
    Indexed(Index),
}

impl Index {
    fn probe(prefix: &[u8]) -> Probe {
        match mp4_index::probe(prefix) {
            mp4_index::Probe::Indexed(index) => return Probe::Indexed(Self::Mp4(index)),
            mp4_index::Probe::NeedsBytes(bytes) => return Probe::NeedsBytes(bytes),
            mp4_index::Probe::Unindexed => {}
        }
        match webm_index::probe(prefix) {
            webm_index::Probe::Indexed(index) => Probe::Indexed(Self::WebM(index)),
            webm_index::Probe::NeedsBytes(bytes) => Probe::NeedsBytes(bytes),
            webm_index::Probe::Unindexed => Probe::Unindexed,
        }
    }

    /// File offset to read from to play from `position`; fragments and clusters start with a
    /// keyframe.
    fn keyframe_offset(&self, position: Duration) -> u64 {
        match self {
            Self::Mp4(index) => index.fragment_offset(position),
            Self::WebM(index) => index.cluster_offset(position),
        }
    }
}

/// Short description of a stream URL for logs: host plus the `itag`/`clen` query parameters
/// when present (never the whole URL, which carries signatures).
fn describe(url: &str) -> String {
    let Ok(url) = reqwest::Url::parse(url) else { return "invalid URL".into() };
    let params: Vec<String> = url
        .query_pairs()
        .filter(|(key, _)| key == "itag" || key == "clen")
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    format!("{} {}", url.host_str().unwrap_or("?"), params.join(" "))
}

fn mark_eos(state: &Mutex<RangeState>, epoch: u64, appsrc: &gst_app::AppSrc) {
    let current = {
        let mut state = lock(state);
        if state.epoch == epoch {
            state.eos = true;
        }
        state.epoch
    };
    if current == epoch {
        // Fails only when the pipeline is already shutting down.
        appsrc.end_of_stream().ok();
    }
}

async fn fetch_range(client: &reqwest::Client, location: &Location, start: u64, end: u64) -> Result<Fetched> {
    let mut request = client.get(&location.url);
    for (name, value) in &location.headers {
        request = request.header(name, value);
    }
    let response =
        request.header(reqwest::header::RANGE, format!("bytes={start}-{end}")).timeout(REQUEST_TIMEOUT).send().await?;
    match response.status() {
        reqwest::StatusCode::RANGE_NOT_SATISFIABLE => Ok(Fetched::PastEnd),
        reqwest::StatusCode::FORBIDDEN => Ok(Fetched::Forbidden),
        _ => Ok(Fetched::Data(response.error_for_status()?.bytes().await?.to_vec())),
    }
}
