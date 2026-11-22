//! The interface every video site implements. Adding a site means implementing [`VideoSource`]
//! and registering it in [`super::Backend::new`] (and [`claims_url`] for browser handoff);
//! nothing else in moth knows which sites exist.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;

use super::media::{
    AlternateUrls, AudioStream, Link, StreamCatalog, VideoDetails, VideoStream, VideoSummary, Watching,
};
use super::selection::StreamPreferences;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'static>>;

/// A video site. Futures run on the backend's tokio runtime, so they may use tokio and reqwest
/// freely; blocking work belongs in `tokio::task::spawn_blocking`.
pub trait VideoSource: Send + Sync {
    /// Stable identifier, used in [`super::media::VideoRef::source`] and reported over IPC, e.g. `youtube`.
    fn id(&self) -> &'static str;

    /// Display name, e.g. "YouTube".
    fn name(&self) -> &'static str;

    /// Cheap, offline check: whether `input` (a URL, or whatever the user pasted) looks like
    /// something this source can open.
    fn claims(&self, input: &str) -> bool;

    /// Turn user input (a URL or bare ID) into a video. Errors are shown to the user.
    fn resolve(&self, input: String) -> BoxFuture<Link>;

    fn details(&self, id: String) -> BoxFuture<VideoDetails>;

    /// Playable streams. Sources that can't list everything (and must pick up front) go by
    /// `preferences`; the rest return every stream and leave choosing to the caller.
    fn streams(&self, id: String, preferences: StreamPreferences) -> BoxFuture<StreamCatalog>;

    /// Other URLs for the same files, for catalogs with `has_alternates`: the player switches to
    /// them if the primary URLs stop serving (e.g. after a long seek).
    fn alternate_urls(
        &self,
        _id: String,
        _video: &VideoStream,
        _audio: &AudioStream,
    ) -> Option<BoxFuture<AlternateUrls>> {
        None
    }

    /// Link to the video on the site.
    fn web_url(&self, id: &str) -> String;

    fn thumbnail_url(&self, _id: &str) -> Option<String> {
        None
    }

    /// Videos to put on the home page right away (known without asking a server).
    fn featured(&self) -> Vec<VideoSummary> {
        Vec::new()
    }

    /// Tell the site how far a video has been watched, if it keeps a history (for a signed-in
    /// account, say). Errors are only logged.
    fn watching(&self, _id: String, _watching: Watching) -> Option<BoxFuture<()>> {
        None
    }

    /// The site's recommendations for the home page, if it has any.
    fn recommendations(&self) -> Option<BoxFuture<Vec<VideoSummary>>> {
        None
    }
}

/// Whether any known source claims `url`. Works without starting the backend (for the browser
/// extension's native-messaging host).
pub fn claims_url(url: &str) -> bool {
    super::youtube::claims_url(url)
}
