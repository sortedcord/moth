//! An `appsrc` that fetches a stream over HTTP in bounded byte ranges.
//!
//! Why not GStreamer's own HTTP source: YouTube's CDN rejects requests without a bounded `Range`
//! (even `bytes=0-` gets a 403), so this feeds the pipeline with bounded ranged requests, much
//! like yt-dlp downloads.
//!
//! Some servers stop serving part-way (YouTube's iOS-client URLs only serve a little ahead of
//! real-time playback, so a long seek gets a 403). If the stream has an alternate URL for the same
//! bytes, a refused request switches to it and carries on; while one is still being resolved, the
//! request waits for it a while.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::stats::NetStats;
use crate::backend::Http;
use crate::backend::media::Location;
use crate::sync::lock;

const MIN_CHUNK_BYTES: u64 = 64 << 10;
const MAX_CHUNK_BYTES: u64 = 1 << 20;
const DEFAULT_CHUNK_BYTES: u64 = 512 << 10;
/// Each request asks for about this much media, when the bitrate is known.
const CHUNK_DURATION: Duration = Duration::from_secs(10);
/// How long a refused request waits for an alternate URL that's still being resolved.
const ALTERNATE_WAIT: Duration = Duration::from_secs(20);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

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
}

impl Endpoint {
    pub fn new(location: Location, alternate_pending: bool) -> Arc<Mutex<Self>> {
        let alternate = if alternate_pending { Alternate::Pending } else { Alternate::Unavailable };
        Arc::new(Mutex::new(Self { location, alternate, switched: false, net: NetStats::default() }))
    }
}

struct RangeState {
    position: u64,
    /// Bumped on every seek, so a fetch that was in flight during the seek is discarded.
    epoch: u64,
    eos: bool,
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

pub fn element(http: Http, endpoint: Arc<Mutex<Endpoint>>, size: Option<u64>, bitrate: Option<u32>) -> gst::Element {
    let chunk = chunk_size(bitrate);
    let appsrc = gst_app::AppSrc::builder()
        .stream_type(gst_app::AppStreamType::Seekable)
        .format(gst::Format::Bytes)
        .size(size.map_or(-1, |size| size as i64))
        .max_bytes(2 * chunk)
        .build();
    let state = Arc::new(Mutex::new(RangeState { position: 0, epoch: 0, eos: false }));
    let fetcher = Fetcher { http, endpoint, size, chunk };

    appsrc.set_callbacks(
        gst_app::AppSrcCallbacks::builder()
            .need_data({
                let state = state.clone();
                move |appsrc, _| {
                    if let Err(err) = fetcher.feed(appsrc, &state) {
                        eprintln!("moth: stream fetch failed: {err:#}");
                        let message = gst::message::Error::builder(gst::ResourceError::Read, &format!("{err:#}"))
                            .src(appsrc)
                            .build();
                        appsrc.post_message(message).ok();
                    }
                }
            })
            .seek_data(move |_, offset| {
                let mut state = lock(&state);
                state.position = offset;
                state.epoch += 1;
                state.eos = false;
                true
            })
            .build(),
    );
    appsrc.upcast()
}

struct Fetcher {
    http: Http,
    endpoint: Arc<Mutex<Endpoint>>,
    size: Option<u64>,
    chunk: u64,
}

impl Fetcher {
    /// Fetch and push the next chunk. Runs on appsrc's streaming thread, so blocking is fine.
    fn feed(&self, appsrc: &gst_app::AppSrc, state: &Mutex<RangeState>) -> Result<()> {
        let (start, epoch) = {
            let state = lock(state);
            if state.eos {
                return Ok(());
            }
            (state.position, state.epoch)
        };
        if self.size.is_some_and(|size| start >= size) {
            mark_eos(state, epoch, appsrc);
            return Ok(());
        }
        let mut end = start + self.chunk - 1;
        if let Some(size) = self.size {
            end = end.min(size - 1);
        }

        let waiting_since = Instant::now();
        let bytes = loop {
            let location = lock(&self.endpoint).location.clone();
            let requested_at = Instant::now();
            let fetched = self.http.runtime.block_on(fetch_range(&self.http.client, &location, start, end))?;
            self.record(&fetched, requested_at.elapsed());
            match fetched {
                Fetched::Data(bytes) => break bytes,
                Fetched::PastEnd => {
                    mark_eos(state, epoch, appsrc);
                    return Ok(());
                }
                Fetched::Forbidden => self.switch_or_wait(waiting_since, start, end)?,
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
        // Pushing fails only when flushing or shutting down; the next need-data carries on.
        if appsrc.push_buffer(buffer).is_ok() && {
            let state = lock(state);
            state.eos && state.epoch == epoch
        } {
            appsrc.end_of_stream().ok();
        }
        Ok(())
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

    /// After a refused request: switch to the alternate URL, wait for it, or give up.
    fn switch_or_wait(&self, waiting_since: Instant, start: u64, end: u64) -> Result<()> {
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
            }
            Alternate::Pending if waiting_since.elapsed() < ALTERNATE_WAIT => {
                endpoint.alternate = Alternate::Pending;
                drop(endpoint);
                std::thread::sleep(Duration::from_millis(200));
            }
            Alternate::Pending | Alternate::Unavailable => {
                bail!("the server refused bytes {start}-{end} of {} (HTTP 403)", describe(&endpoint.location.url))
            }
        }
        Ok(())
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
