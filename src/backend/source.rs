//! The interface every video site implements. Adding a site means implementing [`VideoSource`]
//! and registering it in [`super::Backend::new`] (and [`claims_url`] for browser handoff);
//! nothing else in moth knows which sites exist.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;

use super::media::{
    AlternateUrls, AudioStream, ChannelDetails, ChannelMore, ChannelOrder, ChannelVideos, Link, SearchMore, SearchPage,
    StreamCatalog, VideoDetails, VideoStream, VideoSummary, Watching,
};
use super::music::{
    MusicSearchFilter, MusicSearchMore, MusicSearchPage, MusicSearchSuggestions, MusicShelf, PlayTarget, QueuePage,
    Rating,
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

    /// The site's music home, if it has music.
    fn music_home(&self) -> Option<BoxFuture<Vec<MusicShelf>>> {
        None
    }

    /// The play queue `target` starts.
    fn music_queue(&self, _target: PlayTarget) -> Option<BoxFuture<QueuePage>> {
        None
    }

    /// More of a queue (see [`QueuePage::more`]).
    fn music_queue_more(&self, _more: String) -> Option<BoxFuture<QueuePage>> {
        None
    }

    /// What the signed-in listener thinks of a track, if the site knows.
    fn rating(&self, _id: String) -> Option<BoxFuture<Rating>> {
        None
    }

    fn rate(&self, _id: String, _rating: Rating) -> Option<BoxFuture<()>> {
        None
    }

    /// The site's recommendations for the home page, if it has any.
    fn recommendations(&self) -> Option<BoxFuture<Vec<VideoSummary>>> {
        None
    }

    /// Search for videos matching `query`.
    fn search(&self, _query: String) -> Option<BoxFuture<SearchPage>> {
        None
    }

    /// Next page of video search results (see [`SearchPage::more`]).
    fn search_more(&self, _more: SearchMore) -> Option<BoxFuture<SearchPage>> {
        None
    }

    /// Autocomplete suggestions for a partial video search query.
    fn search_suggestions(&self, _query: String) -> Option<BoxFuture<Vec<String>>> {
        None
    }

    /// Whether `input` looks like a link to one of the site's channels (or a handle such as
    /// `@name`), as opposed to a video.
    fn claims_channel(&self, _input: &str) -> bool {
        false
    }

    /// The id of the channel a link or handle points at.
    fn resolve_channel(&self, _input: String) -> Option<BoxFuture<String>> {
        None
    }

    /// Whether the site has channel pages (the UI only makes channel names links if so).
    fn has_channels(&self) -> bool {
        false
    }

    /// The id of the channel that posted a video.
    fn channel_of(&self, _video_id: String) -> Option<BoxFuture<String>> {
        None
    }

    /// A channel's page, with its latest videos.
    fn channel(&self, _id: String) -> Option<BoxFuture<ChannelDetails>> {
        None
    }

    /// A channel's videos in the given order.
    fn channel_videos(&self, _id: String, _order: ChannelOrder) -> Option<BoxFuture<ChannelVideos>> {
        None
    }

    /// Next page of a channel's videos (see [`ChannelVideos::more`]).
    fn channel_videos_more(&self, _more: ChannelMore) -> Option<BoxFuture<ChannelVideos>> {
        None
    }

    /// Subscribe the signed-in account to a channel, or unsubscribe it.
    fn subscribe(&self, _channel_id: String, _subscribed: bool) -> Option<BoxFuture<()>> {
        None
    }

    /// Search the site's music catalog.
    fn music_search(&self, _query: String, _filter: MusicSearchFilter) -> Option<BoxFuture<MusicSearchPage>> {
        None
    }

    /// Next page of music search results (see [`MusicSearchPage::more`]).
    fn music_search_more(&self, _more: MusicSearchMore) -> Option<BoxFuture<MusicSearchPage>> {
        None
    }

    /// Autocomplete suggestions for a partial music search query.
    fn music_search_suggestions(&self, _query: String) -> Option<BoxFuture<MusicSearchSuggestions>> {
        None
    }
}

/// Whether any known source claims `url`. Works without starting the backend (for the browser
/// extension's native-messaging host).
pub fn claims_url(url: &str) -> bool {
    super::youtube::claims_url(url)
}
