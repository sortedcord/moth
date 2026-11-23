//! Everything that isn't UI: video sources, the media model, stream selection and playback.
//!
//! Nothing in here depends on gpui. The UI talks to sites only through [`Backend`] and the
//! neutral types in [`media`], and to GStreamer only through [`player`].

pub mod accounts;
mod local;
pub mod media;
pub mod music;
pub mod player;
mod secrets;
pub mod selection;
pub mod source;
mod youtube;

use std::future::Future;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::dev::DevOptions;
use accounts::{AccountInfo, DeviceCode, Role};
use media::{AlternateUrls, Link, Selection, StreamCatalog, VideoDetails, VideoRef, VideoSummary, Watching};
use music::{MusicShelf, PlayTarget, QueuePage, Rating};
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
    /// The only source with accounts so far.
    youtube: Arc<youtube::YouTube>,
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
        let youtube =
            Arc::new(runtime.block_on(youtube::YouTube::new(http.clone(), resolver(dev), dev.ytdlp_format.clone()))?);
        let mut sources: Vec<Arc<dyn VideoSource>> = vec![youtube.clone()];
        if let Some(dir) = &dev.media_dir {
            let _runtime = runtime.enter();
            match local::Local::new(dir) {
                Ok(Some(local)) => sources.push(Arc::new(local)),
                Ok(None) => {}
                Err(err) => eprintln!("moth: local videos unavailable: {err:#}"),
            }
        }
        Ok(Arc::new(Self { runtime, http, sources, youtube }))
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
        let video_stream = catalog.video_stream(selection.video?)?;
        let audio_stream = catalog.audio_stream(selection.audio)?;
        let future = source.alternate_urls(video.id.clone(), video_stream, audio_stream)?;
        Some(self.run(future))
    }

    /// Videos for the home page that are known right away: each source's featured ones (local
    /// dev videos first).
    pub fn featured(&self) -> Vec<VideoSummary> {
        self.sources.iter().rev().flat_map(|source| source.featured()).collect()
    }

    /// The sites' recommendations for the home page, one after another. Fails only if every
    /// site that has recommendations fails.
    pub fn recommendations(&self) -> impl Future<Output = Result<Vec<VideoSummary>>> + use<> {
        let feeds: Vec<_> =
            self.sources.iter().filter_map(|source| Some((source.name(), source.recommendations()?))).collect();
        self.run(async move {
            let mut videos = Vec::new();
            let mut errors = Vec::new();
            for (name, feed) in feeds {
                match feed.await {
                    Ok(feed) => videos.extend(feed),
                    Err(err) => errors.push(format!("{name}: {err:#}")),
                }
            }
            if videos.is_empty() && !errors.is_empty() {
                bail!("{}", errors.join("\n"));
            }
            Ok(videos)
        })
    }

    /// Report progress through `video` to its site (history); failures are logged.
    pub fn watching(&self, video: &VideoRef, watching: Watching) {
        let Some(report) =
            self.source(video.source).ok().and_then(|source| source.watching(video.id.clone(), watching))
        else {
            return;
        };
        self.runtime.spawn(async move {
            if let Err(err) = report.await {
                eprintln!("moth: couldn't update the watch history: {err:#}");
            }
        });
    }

    // ── Music ────────────────────────────────────────────────────────────────

    /// The music home (YouTube Music's; the only site with music so far).
    pub fn music_home(&self) -> impl Future<Output = Result<Vec<MusicShelf>>> + use<> {
        let home = self.youtube.music_home();
        self.run(async move { home.ok_or_else(|| anyhow!("no music"))?.await })
    }

    pub fn music_queue(&self, target: PlayTarget) -> impl Future<Output = Result<QueuePage>> + use<> {
        let queue = self.source(target.source).ok().and_then(|source| source.music_queue(target));
        self.run(async move { queue.ok_or_else(|| anyhow!("that site has no music"))?.await })
    }

    pub fn music_queue_more(&self, source: &str, more: String) -> impl Future<Output = Result<QueuePage>> + use<> {
        let queue = self.source(source).ok().and_then(|source| source.music_queue_more(more));
        self.run(async move { queue.ok_or_else(|| anyhow!("that site has no music"))?.await })
    }

    /// The signed-in listener's rating of a track; `None` (as a rating) when nobody's signed in.
    pub fn rating(&self, video: &VideoRef) -> impl Future<Output = Result<Rating>> + use<> {
        let rating = self.source(video.source).ok().and_then(|source| source.rating(video.id.clone()));
        self.run(async move {
            match rating {
                Some(rating) => rating.await,
                None => Ok(Rating::None),
            }
        })
    }

    pub fn rate(&self, video: &VideoRef, rating: Rating) -> impl Future<Output = Result<()>> + use<> {
        let rate = self.source(video.source).ok().and_then(|source| source.rate(video.id.clone(), rating));
        self.run(async move { rate.ok_or_else(|| anyhow!("sign in a recommendation account to rate tracks"))?.await })
    }

    // ── Accounts ─────────────────────────────────────────────────────────────

    /// Who's signed in for `role`, if anyone.
    pub fn account(&self, role: Role) -> Option<AccountInfo> {
        self.youtube.account(role).info()
    }

    /// Look up the signed-in accounts' names (at startup they're only known to be signed in).
    /// Returns the accounts that were signed out because their session expired, and why.
    pub fn refresh_accounts(&self) -> impl Future<Output = Result<Vec<(Role, String)>>> + use<> {
        let accounts: Vec<_> = Role::ALL.iter().map(|role| self.youtube.account(*role)).collect();
        self.run(async move {
            let mut signed_out = Vec::new();
            for account in accounts.iter().filter(|account| account.method().is_some()) {
                match account.refresh_info().await {
                    Ok(_) => {}
                    Err(err) if err.is::<youtube::SessionExpired>() => {
                        signed_out.push((account.role(), err.to_string()))
                    }
                    Err(err) => {
                        eprintln!(
                            "moth: couldn't look up the YouTube {}: {err:#}",
                            account.role().name().to_lowercase()
                        )
                    }
                }
            }
            Ok(signed_out)
        })
    }

    /// Start a device sign-in for `role`; show the code, then await [`Self::finish_device_sign_in`].
    pub fn begin_device_sign_in(&self, role: Role) -> impl Future<Output = Result<DeviceCode>> + use<> {
        let account = self.youtube.account(role);
        self.run(async move { account.begin_device_sign_in().await })
    }

    pub fn finish_device_sign_in(&self, role: Role) -> impl Future<Output = Result<AccountInfo>> + use<> {
        let account = self.youtube.account(role);
        self.run(async move { account.finish_device_sign_in().await })
    }

    pub fn cancel_device_sign_in(&self, role: Role) {
        self.youtube.account(role).cancel_device_sign_in();
    }

    /// Sign `role` in with a browser session's cookies (a `Cookie` header or a cookies.txt).
    pub fn sign_in_with_cookies(
        &self,
        role: Role,
        cookies: String,
    ) -> impl Future<Output = Result<AccountInfo>> + use<> {
        let account = self.youtube.account(role);
        self.run(async move { account.sign_in_with_cookies(&cookies).await })
    }

    pub fn sign_out(&self, role: Role) -> impl Future<Output = Result<()>> + use<> {
        let account = self.youtube.account(role);
        self.run(async move { account.sign_out().await })
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
