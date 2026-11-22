//! Everything that isn't UI: video sources, the media model, stream selection and playback.
//!
//! Nothing in here depends on gpui. The UI talks to sites only through [`Backend`] and the
//! neutral types in [`media`], and to GStreamer only through [`player`].

mod local;
pub mod media;
pub mod player;
pub mod samples;
pub mod selection;
pub mod source;
mod youtube;

use std::future::Future;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::dev::DevOptions;
use media::{AlternateUrls, Link, Selection, StreamCatalog, VideoDetails, VideoRef, VideoSummary};
use selection::StreamPreferences;
use source::VideoSource;

/// An HTTP client and the tokio runtime it runs on.
#[derive(Clone)]
pub struct Http {
    pub client: reqwest::Client,
    pub runtime: tokio::runtime::Handle,
}

pub struct Backend {
    runtime: tokio::runtime::Runtime,
    http: reqwest::Client,
    sources: Vec<Arc<dyn VideoSource>>,
}

/// `MOTH_RESOLVER`, if set to one that exists.
fn resolver(dev: &DevOptions) -> youtube::Resolver {
    let Some(name) = &dev.resolver else { return youtube::Resolver::default() };
    youtube::Resolver::parse(name).unwrap_or_else(|| {
        eprintln!("moth: unknown MOTH_RESOLVER {name:?}; expected visionos, rustypipe or yt-dlp");
        youtube::Resolver::default()
    })
}

impl Backend {
    pub fn new(dev: &DevOptions) -> Result<Arc<Self>> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("moth-net")
            .enable_all()
            .build()
            .context("failed to start tokio runtime")?;
        let http = reqwest::Client::builder().build()?;
        let youtube = runtime.block_on(youtube::YouTube::new(http.clone(), resolver(dev), dev.ytdlp_format.clone()))?;
        let mut sources: Vec<Arc<dyn VideoSource>> = vec![Arc::new(youtube)];
        if let Some(dir) = &dev.media_dir {
            let _runtime = runtime.enter();
            match local::Local::new(dir) {
                Ok(Some(local)) => sources.push(Arc::new(local)),
                Ok(None) => {}
                Err(err) => eprintln!("moth: local videos unavailable: {err:#}"),
            }
        }
        Ok(Arc::new(Self { runtime, http, sources }))
    }

    pub fn http(&self) -> Http {
        Http { client: self.http.clone(), runtime: self.runtime.handle().clone() }
    }

    /// Run `future` on the tokio runtime; the returned future can be awaited from any executor.
    fn run<T, F>(&self, future: F) -> impl Future<Output = Result<T>> + use<T, F>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let handle = self.runtime.spawn(future);
        async move { handle.await.map_err(|err| anyhow!("background task failed: {err}"))? }
    }

    fn source(&self, id: &str) -> Result<&Arc<dyn VideoSource>> {
        self.sources.iter().find(|source| source.id() == id).ok_or_else(|| anyhow!("unknown video source `{id}`"))
    }

    /// The video a pasted link (or ID) refers to, from whichever source claims it.
    pub fn resolve(&self, input: String) -> impl Future<Output = Result<Link>> + use<> {
        let input = input.trim().to_string();
        let source = self.sources.iter().find(|source| source.claims(&input)).cloned();
        let names: Vec<_> = self.sources.iter().map(|source| source.name()).collect();
        self.run(async move {
            if input.is_empty() {
                bail!("Enter a video link");
            }
            match source {
                Some(source) => source.resolve(input).await,
                None => bail!("Not a link moth can open (supported: {})", names.join(", ")),
            }
        })
    }

    pub fn details(&self, video: &VideoRef) -> impl Future<Output = Result<VideoDetails>> + use<> {
        let future = self.source(video.source).map(|source| source.details(video.id.clone()));
        self.run(async move { future?.await })
    }

    pub fn streams(
        &self,
        video: &VideoRef,
        preferences: StreamPreferences,
    ) -> impl Future<Output = Result<StreamCatalog>> + use<> {
        let future = self.source(video.source).map(|source| source.streams(video.id.clone(), preferences));
        self.run(async move { future?.await })
    }

    /// Alternate URLs for a selection, if the catalog's source offers them.
    pub fn alternate_urls(
        &self,
        video: &VideoRef,
        catalog: &StreamCatalog,
        selection: Selection,
    ) -> Option<impl Future<Output = Result<AlternateUrls>> + use<>> {
        if !catalog.has_alternates {
            return None;
        }
        let source = self.source(video.source).ok()?;
        let video_stream = catalog.video_stream(selection.video)?;
        let audio_stream = catalog.audio_stream(selection.audio)?;
        let future = source.alternate_urls(video.id.clone(), video_stream, audio_stream)?;
        Some(self.run(future))
    }

    /// The home page: each source's featured videos (local dev videos first), then the samples.
    pub fn home_feed(&self) -> Vec<VideoSummary> {
        let mut feed: Vec<_> = self.sources.iter().rev().flat_map(|source| source.featured()).collect();
        feed.extend(samples::home_feed());
        feed
    }

    pub fn web_url(&self, video: &VideoRef) -> Option<String> {
        Some(self.source(video.source).ok()?.web_url(&video.id))
    }

    pub fn thumbnail_url(&self, video: &VideoRef) -> Option<String> {
        self.source(video.source).ok()?.thumbnail_url(&video.id)
    }

    pub fn fetch_bytes(&self, url: String) -> impl Future<Output = Result<Vec<u8>>> + use<> {
        let http = self.http.clone();
        self.run(async move {
            let response = http.get(&url).send().await?.error_for_status()?;
            Ok(response.bytes().await?.to_vec())
        })
    }
}
