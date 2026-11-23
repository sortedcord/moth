//! Music, as the UI sees it: a site's music home (shelves of songs, albums, playlists and
//! artists) and play queues. Only YouTube Music so far.

use std::time::Duration;

use super::media::VideoRef;

/// A row of the music home page, e.g. "Listen again" or "Quick picks".
#[derive(Clone, Debug)]
pub struct MusicShelf {
    pub title: String,
    pub layout: ShelfLayout,
    pub items: Vec<MusicItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShelfLayout {
    /// Square (or round, for artists) cards in a row.
    Cards,
    /// Song rows with a small thumbnail, in columns.
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicKind {
    Song,
    Video,
    Album,
    Playlist,
    Artist,
}

#[derive(Clone, Debug)]
pub struct MusicItem {
    pub kind: MusicKind,
    pub title: String,
    /// e.g. "Album · Artist · 2019", "Artist · 1.2M plays".
    pub subtitle: String,
    pub thumbnail_url: Option<String>,
    /// What playing it starts; `None` for items that can't be played (some artists).
    pub play: Option<PlayTarget>,
}

/// Where a queue starts: a track, a playlist (an album, a mix), or a track within a playlist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayTarget {
    /// The site, as in [`VideoRef::source`].
    pub source: &'static str,
    pub track: Option<String>,
    pub playlist: Option<String>,
}

/// One entry of a play queue.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub video: VideoRef,
    pub title: String,
    /// e.g. "Rita & VISUAL ARTS / Key".
    pub artists: String,
    /// The rest of the byline, e.g. "Little Busters! Original SoundTrack · 2007".
    pub detail: Option<String>,
    pub duration: Option<Duration>,
    /// Small, for lists and the now-playing bar.
    pub thumbnail_url: Option<String>,
    /// The largest there is, for the expanded player.
    pub artwork_url: Option<String>,
}

/// Part of a play queue; radio-style queues go on (see `more`).
#[derive(Clone, Debug)]
pub struct QueuePage {
    pub tracks: Vec<Track>,
    /// Fetches the next page (opaque to everyone but the source).
    pub more: Option<String>,
}

/// What the listener thinks of a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Rating {
    Like,
    Dislike,
    #[default]
    None,
}
