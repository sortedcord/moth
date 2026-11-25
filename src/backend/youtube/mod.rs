//! YouTube, via YouTube's internal InnerTube API: RustyPipe for video details, related videos
//! and the home feed; streams from the first of these that works (see [`Resolver`]):
//!
//! 1. The visionOS client ([`visionos`]), whose URLs serve any byte range from the start.
//! 2. RustyPipe, which gets streams from YouTube's iOS client. Those URLs only serve a little
//!    ahead of real-time playback, so the catalog offers web-client URLs for the same files as
//!    alternate URLs (resolved by yt-dlp), which the player switches to when a long seek gets
//!    refused.
//! 3. yt-dlp alone.
//!
//! A playback account signed in with a Premium browser session adds Premium's streams to
//! whichever catalog results (see [`premium_streams`]).
//!
//! yt-dlp's URLs have a catch of their own: YouTube may only start serving them a few seconds
//! after handing them out. They leave this module once they work (see [`resolve_with_ytdlp`]).

mod account;
pub use account::SessionExpired;
mod channel;
mod feed;
mod history;
mod music;
mod visionos;
mod ytdlp;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use rustypipe::client::RustyPipe;
use rustypipe::model::paginator::ContinuationEndpoint;
use rustypipe::model::richtext::ToPlaintext as _;
use rustypipe::model::{
    AudioCodec as RpAudioCodec, Thumbnail, UrlTarget, VideoCodec as RpVideoCodec, VideoItem, VideoPlayer,
};

use super::accounts::{Method, Role};
use super::media::{
    AlternateUrls, AudioCodec, AudioStream, AudioTrack, Channel, ChannelDetails, ChannelMore, ChannelOrder,
    ChannelVideos, Link, Location, Published, SearchMore, SearchPage, StreamCatalog, StreamId, VideoCodec,
    VideoDetails, VideoRef, VideoStream, VideoSummary, Watching,
};
use super::music::{
    MusicSearchFilter, MusicSearchMore, MusicSearchPage, MusicSearchSuggestions, MusicShelf, PlayTarget, QueuePage,
    Rating,
};
use super::selection::StreamPreferences;
use super::source::{BoxFuture, VideoSource};

pub const ID: &str = "youtube";

const HOSTS: [&str; 7] = [
    "youtube.com",
    "www.youtube.com",
    "m.youtube.com",
    "music.youtube.com",
    "youtu.be",
    "youtube-nocookie.com",
    "www.youtube-nocookie.com",
];

/// Whether `url` is a link to YouTube.
pub fn claims_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    HOSTS.contains(&host)
}

/// Scheme-less YouTube links like `youtube.com/watch?v=…` or `youtu.be/…`.
fn claims_host_path(input: &str) -> bool {
    let Some((host, _)) = input.split_once('/') else { return false };
    HOSTS.contains(&host)
}

/// YouTube video IDs: 11 characters of URL-safe base64. Plain words of 11 letters (like
/// "programming" or "PlayStation") are left to search rather than treated as IDs.
fn is_video_id(input: &str) -> bool {
    if input.len() != 11 || !input.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_') {
        return false;
    }
    let has_non_alpha = input.bytes().any(|byte| byte.is_ascii_digit() || byte == b'-' || byte == b'_');
    let upper = input.bytes().filter(u8::is_ascii_uppercase).count();
    let lower = input.bytes().filter(u8::is_ascii_lowercase).count();
    has_non_alpha || (upper >= 3 && lower > 0)
}

/// Where stream URLs come from, in the order they're tried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Resolver {
    #[default]
    VisionOs,
    RustyPipe,
    YtDlp,
}

impl Resolver {
    /// `visionos`, `rustypipe` or `yt-dlp`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "visionos" => Some(Self::VisionOs),
            "rustypipe" => Some(Self::RustyPipe),
            "yt-dlp" | "ytdlp" => Some(Self::YtDlp),
            _ => None,
        }
    }
}

const LIVE: &str = "Live streams aren't supported yet";

pub struct YouTube {
    client: RustyPipe,
    /// Signed-in accounts, by role (see [`crate::backend::accounts`]).
    accounts: [Arc<account::Account>; 2],
    history: Arc<history::History>,
    /// What the account just subscribed to or left: YouTube's pages report it a few seconds late.
    subscriptions: Arc<channel::RecentSubscriptions>,
    http: reqwest::Client,
    /// Dev override: start the resolver chain further down.
    first: Resolver,
    /// Dev override for yt-dlp's format selection.
    ytdlp_format: Option<String>,
}

impl YouTube {
    /// Must run inside the tokio runtime: RustyPipe's builder spawns background work.
    pub async fn new(http: reqwest::Client, first: Resolver, ytdlp_format: Option<String>) -> Result<Self> {
        let storage_dir = crate::paths::cache_dir();
        std::fs::create_dir_all(&storage_dir)
            .map_err(|err| anyhow!("failed to create {}: {err}", storage_dir.display()))?;
        let client = RustyPipe::builder().storage_dir(storage_dir).build()?;
        let accounts = [
            Arc::new(account::Account::load(Role::Playback).await?),
            Arc::new(account::Account::load(Role::Recommendations).await?),
        ];
        Ok(Self {
            client,
            accounts,
            history: Default::default(),
            subscriptions: Default::default(),
            http,
            first,
            ytdlp_format,
        })
    }

    pub fn account(&self, role: Role) -> Arc<account::Account> {
        self.accounts.iter().find(|account| account.role() == role).expect("an account per role").clone()
    }
}

const SIGN_IN_HINT: &str = "This video needs you signed in: sign in a playback account with a browser \
                            session in Settings → YouTube accounts.";

/// Streams for a video YouTube only plays to a signed-in viewer, through the playback account
/// (if it's signed in). That takes a browser session: yt-dlp signs in with its cookies. A code
/// sign-in (the TV app) only gets SABR streams, which moth can't play yet (#11).
async fn playback_streams(
    account: &account::Account,
    id: &str,
    preferences: &StreamPreferences,
    ytdlp_format: Option<String>,
) -> Option<Result<StreamCatalog>> {
    let result = match account.method()? {
        Method::Cookies => {
            let format = ytdlp_format.unwrap_or_else(|| ytdlp::format_for(preferences));
            resolve_with_ytdlp(id.to_string(), format, account.cookie_header()).await.map(|mut catalog| {
                catalog.resolver_detail = "yt-dlp (playback account)".to_string();
                catalog
            })
        }
        Method::DeviceCode => Err(anyhow!(
            "a playback account signed in with a code can't play videos that need signing in yet; \
             sign it in with a browser session instead"
        )),
    };
    Some(result)
}

/// The itags of the 256 kbps audio only Premium accounts get.
const PREMIUM_AUDIO_ITAGS: [u64; 2] = [141, 774];

/// The longest moth waits for Premium's streams before playing without them.
const PREMIUM_TIMEOUT: Duration = Duration::from_secs(45);

/// A video's Premium-only streams (see [`ytdlp::premium`]), if the playback account is signed
/// in with a browser session that has Premium and the video has any. Failing to get them only
/// means playing the usual streams.
async fn premium_streams(account: Arc<account::Account>, id: String) -> Option<StreamCatalog> {
    if account.method() != Some(Method::Cookies) || !account.is_premium().await {
        return None;
    }
    let cookies = account.cookie_header()?;
    let resolve = tokio::task::spawn_blocking(move || {
        let cookie_file = ytdlp::CookieFile::new(&cookies)?;
        ytdlp::premium(&id, cookie_file.path())
    });
    let resolved = match tokio::time::timeout(PREMIUM_TIMEOUT, resolve).await {
        Ok(Ok(Ok(resolved))) => resolved,
        Ok(Ok(Err(err))) => {
            eprintln!("moth: no Premium streams: {err:#}");
            return None;
        }
        Ok(Err(err)) => {
            eprintln!("moth: no Premium streams: {err}");
            return None;
        }
        Err(_) => {
            eprintln!("moth: no Premium streams: yt-dlp took more than {}s", PREMIUM_TIMEOUT.as_secs());
            return None;
        }
    };
    if resolved.catalog.video.is_empty() && resolved.catalog.audio.is_empty() {
        eprintln!("moth: yt-dlp found no Premium streams for this video");
        return None;
    }
    let wait = resolved.usable_in();
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    Some(resolved.catalog)
}

/// Offer `premium`'s streams next to the catalog's own.
fn add_premium(catalog: &mut StreamCatalog, premium: StreamCatalog) {
    let before = (catalog.video.len(), catalog.audio.len());
    for stream in premium.video {
        if catalog.video_stream(stream.id).is_none() {
            catalog.video.push(stream);
        }
    }
    // The 256 kbps files have no audio tracks of their own: they belong to a video's only one.
    let tracks = catalog.tracks();
    if tracks.len() <= 1 {
        for mut stream in premium.audio {
            if !catalog.audio.iter().any(|known| itag(known.id) == itag(stream.id)) {
                stream.track = tracks.first().cloned();
                catalog.audio.push(stream);
            }
        }
    }
    if (catalog.video.len(), catalog.audio.len()) != before {
        let added = (catalog.video.len() - before.0, catalog.audio.len() - before.1);
        eprintln!("moth: added {} Premium video and {} Premium audio streams", added.0, added.1);
        catalog.resolver_detail.push_str(" + Premium streams (yt-dlp)");
    }
}

/// The recommendation account's home feed, if it's signed in.
async fn signed_in_home(account: &account::Account) -> Option<Result<Vec<VideoSummary>>> {
    let (query, client) = account.query()?;
    let result = async {
        let response = query.raw(client, "browse", &serde_json::json!({"browseId": "FEwhat_to_watch"})).await?;
        let response: serde_json::Value = serde_json::from_str(&response)?;
        let videos = feed::videos(&response);
        if videos.is_empty() {
            bail!("the home feed is empty");
        }
        Ok(videos)
    };
    Some(result.await)
}

impl VideoSource for YouTube {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "YouTube"
    }

    fn claims(&self, input: &str) -> bool {
        claims_url(input) || claims_host_path(input) || is_video_id(input)
    }

    fn resolve(&self, input: String) -> BoxFuture<Link> {
        let client = self.client.clone();
        Box::pin(async move {
            match client.query().resolve_string(&input, false).await {
                Ok(UrlTarget::Video { id, start_time }) => Ok(Link {
                    video: VideoRef { source: ID, id },
                    start: (start_time > 0).then(|| Duration::from_secs(start_time.into())),
                }),
                Ok(_) => bail!("Only video links are supported for now"),
                Err(_) => bail!("That doesn't look like a YouTube video link"),
            }
        })
    }

    fn details(&self, id: String) -> BoxFuture<VideoDetails> {
        let client = self.client.clone();
        Box::pin(async move {
            let details = client.query().video_details(&id).await?;
            let channel = details.channel;
            Ok(VideoDetails {
                title: details.name,
                channel: Channel {
                    id: Some(channel.id.clone()),
                    avatar_url: pick_thumbnail(&channel.avatar, 80),
                    subscribers: channel.subscriber_count,
                    name: channel.name,
                },
                views: Some(details.view_count),
                likes: details.like_count.map(u64::from),
                published: details
                    .publish_date
                    .map(|date| Published::Date { year: date.year(), month: u8::from(date.month()), day: date.day() })
                    .or(details.publish_date_txt.map(Published::Text)),
                description: details.description.to_plaintext(),
                related: details.recommended.items.into_iter().map(summary).collect(),
            })
        })
    }

    fn streams(&self, id: String, preferences: StreamPreferences) -> BoxFuture<StreamCatalog> {
        let client = self.client.clone();
        let http = self.http.clone();
        let first = self.first;
        let ytdlp_format = self.ytdlp_format.clone();
        let playback = self.account(Role::Playback);
        let premium_account = playback.clone();
        let premium_id = id.clone();
        let usual = async move {
            let mut failures = Vec::new();
            // YouTube wants a signed-in viewer (age-restricted, members-only, private, a bot check).
            let mut needs_sign_in = false;
            if first <= Resolver::VisionOs {
                match visionos::resolve(&http, &client, &id).await {
                    Ok(visionos::Outcome::Streams(mut catalog)) => {
                        number_tracks(&mut catalog);
                        return Ok(catalog);
                    }
                    Ok(visionos::Outcome::Live) => bail!(LIVE),
                    Err(err) => {
                        needs_sign_in = err.downcast_ref::<visionos::Refused>().is_some();
                        eprintln!("moth: the visionOS client gave no streams: {err:#}");
                        failures.push(format!("visionOS client: {err:#}"));
                    }
                }
            }
            if needs_sign_in {
                match playback_streams(&playback, &id, &preferences, ytdlp_format.clone()).await {
                    Some(Ok(catalog)) => return Ok(catalog),
                    Some(Err(err)) => {
                        eprintln!("moth: the playback account couldn't play it either: {err:#}");
                        failures.push(format!("playback account: {err:#}"));
                    }
                    None => {}
                }
            }
            if first <= Resolver::RustyPipe {
                let error = match client.query().player(&id).await {
                    Ok(player) if player.details.is_live => bail!(LIVE),
                    Ok(player) => {
                        let mut catalog = catalog(&player);
                        number_tracks(&mut catalog);
                        if !catalog.video.is_empty() && !catalog.audio.is_empty() {
                            return Ok(catalog);
                        }
                        anyhow!("no playable streams")
                    }
                    Err(err) => err.into(),
                };
                eprintln!("moth: RustyPipe stream resolution failed, trying yt-dlp: {error:#}");
                failures.push(format!("RustyPipe: {error:#}"));
            }
            let format = ytdlp_format.unwrap_or_else(|| ytdlp::format_for(&preferences));
            match resolve_with_ytdlp(id, format, None).await {
                Ok(mut catalog) => {
                    if !failures.is_empty() {
                        catalog.resolver_detail = "yt-dlp (fallback)".to_string();
                    }
                    Ok(catalog)
                }
                Err(error) => {
                    failures.push(format!("yt-dlp: {error:#}"));
                    if needs_sign_in && playback.cookie_header().is_none() {
                        failures.insert(0, SIGN_IN_HINT.to_string());
                    }
                    bail!("{}", failures.join("\n"))
                }
            }
        };
        Box::pin(async move {
            // Premium's streams take yt-dlp a few seconds: ask for them while the usual ones
            // resolve (this runs in the Tokio runtime, unlike the code that makes the future).
            let premium = tokio::spawn(premium_streams(premium_account, premium_id));
            let mut catalog = usual.await?;
            if let Ok(Some(premium)) = premium.await {
                add_premium(&mut catalog, premium);
            }
            Ok(catalog)
        })
    }

    fn alternate_urls(&self, id: String, video: &VideoStream, audio: &AudioStream) -> Option<BoxFuture<AlternateUrls>> {
        let audio_format = match &audio.track {
            // Dubs share the itag; yt-dlp tells them apart by language (and lists the default
            // track's "stable volume" copy under the same itag, as `-drc`).
            Some(track) => {
                let language = track.id.split('.').next().unwrap_or_default();
                format!("(ba[format_id^={itag}-][format_id!$=-drc][language={language}]/{itag})", itag = itag(audio.id))
            }
            None => itag(audio.id).to_string(),
        };
        let format = format!("{}+{audio_format}", itag(video.id));
        // Premium's streams only resolve for the account that has them.
        let cookies = (video.premium || PREMIUM_AUDIO_ITAGS.contains(&itag(audio.id)))
            .then(|| self.account(Role::Playback).cookie_header())
            .flatten();
        Some(Box::pin(async move {
            let catalog = resolve_with_ytdlp(id, format, cookies).await?;
            match (catalog.video.into_iter().next(), catalog.audio.into_iter().next()) {
                (Some(video), Some(audio)) => Ok(AlternateUrls { video: video.location, audio: audio.location }),
                _ => bail!("yt-dlp returned no alternate URLs"),
            }
        }))
    }

    fn music_home(&self) -> Option<BoxFuture<Vec<MusicShelf>>> {
        let (account, client) = (self.account(Role::Recommendations), self.client.clone());
        Some(Box::pin(async move { music::home(&account, &client).await }))
    }

    fn music_queue(&self, target: PlayTarget) -> Option<BoxFuture<QueuePage>> {
        let (account, client) = (self.account(Role::Recommendations), self.client.clone());
        Some(Box::pin(async move { music::queue(&account, &client, &target).await }))
    }

    fn music_queue_more(&self, more: String) -> Option<BoxFuture<QueuePage>> {
        let (account, client) = (self.account(Role::Recommendations), self.client.clone());
        Some(Box::pin(async move { music::queue_more(&account, &client, &more).await }))
    }

    fn music_search(&self, query: String, filter: MusicSearchFilter) -> Option<BoxFuture<MusicSearchPage>> {
        let (account, client) = (self.account(Role::Recommendations), self.client.clone());
        Some(Box::pin(async move { music::search(&account, &client, &query, filter).await }))
    }

    fn music_search_more(&self, more: MusicSearchMore) -> Option<BoxFuture<MusicSearchPage>> {
        let (account, client) = (self.account(Role::Recommendations), self.client.clone());
        Some(Box::pin(async move { music::search_more(&account, &client, &more.token).await }))
    }

    fn music_search_suggestions(&self, query: String) -> Option<BoxFuture<MusicSearchSuggestions>> {
        let (account, client) = (self.account(Role::Recommendations), self.client.clone());
        Some(Box::pin(async move { music::search_suggestions(&account, &client, &query).await }))
    }

    fn rating(&self, id: String) -> Option<BoxFuture<Rating>> {
        let account = self.account(Role::Recommendations);
        account.method()?;
        Some(Box::pin(async move { music::rating(&account, &id).await }))
    }

    fn rate(&self, id: String, rating: Rating) -> Option<BoxFuture<()>> {
        let account = self.account(Role::Recommendations);
        account.method()?;
        Some(Box::pin(async move { music::rate(&account, &id, rating).await }))
    }

    /// Into the recommendation account's history, if it's signed in.
    fn watching(&self, id: String, watching: Watching) -> Option<BoxFuture<()>> {
        let account = self.account(Role::Recommendations);
        account.method()?;
        let (history, http) = (self.history.clone(), self.http.clone());
        Some(Box::pin(async move {
            match watching {
                Watching::Started(position) => history.started(&account, &http, &id, position).await,
                Watching::Progress(position) => history.progress(&account, &http, &id, position).await,
                Watching::Jumped(before, after) => history.jumped(&account, &http, &id, before, after).await,
                Watching::Stopped(position) => {
                    let result = history.progress(&account, &http, &id, position).await;
                    history.stopped(&id);
                    result
                }
            }
        }))
    }

    /// The recommendation account's home feed when it's signed in, else the signed-out one.
    fn recommendations(&self) -> Option<BoxFuture<Vec<VideoSummary>>> {
        let (http, client) = (self.http.clone(), self.client.clone());
        let account = self.account(Role::Recommendations);
        Some(Box::pin(async move {
            match signed_in_home(&account).await {
                Some(Ok(videos)) => return Ok(videos),
                Some(Err(err)) => eprintln!("moth: no signed-in home feed, showing the signed-out one: {err:#}"),
                None => {}
            }
            visionos::home(&http, &client).await
        }))
    }

    fn has_channels(&self) -> bool {
        true
    }

    fn channel_of(&self, video_id: String) -> Option<BoxFuture<String>> {
        let client = self.client.clone();
        Some(Box::pin(async move { Ok(client.query().video_details(&video_id).await?.channel.id) }))
    }

    fn claims_channel(&self, input: &str) -> bool {
        channel::claims(input)
    }

    fn resolve_channel(&self, input: String) -> Option<BoxFuture<String>> {
        let client = self.client.clone();
        Some(Box::pin(async move {
            match client.query().resolve_string(&channel::normalize(&input), false).await {
                Ok(UrlTarget::Channel { id }) => Ok(id),
                Ok(_) => bail!("That isn't a link to a channel"),
                Err(err) => bail!("Couldn't find that channel: {err}"),
            }
        }))
    }

    fn channel(&self, id: String) -> Option<BoxFuture<ChannelDetails>> {
        let (client, account) = (self.client.clone(), self.account(Role::Recommendations));
        let recent = self.subscriptions.clone();
        Some(Box::pin(async move {
            let mut details = channel::details(&client, &account, id.clone()).await?;
            details.subscribed = recent.get(&id).or(details.subscribed);
            Ok(details)
        }))
    }

    fn channel_videos(&self, id: String, order: ChannelOrder) -> Option<BoxFuture<ChannelVideos>> {
        let client = self.client.clone();
        Some(Box::pin(async move { channel::videos(&client, id, order).await }))
    }

    fn channel_videos_more(&self, more: ChannelMore) -> Option<BoxFuture<ChannelVideos>> {
        let client = self.client.clone();
        Some(Box::pin(async move { channel::videos_more(&client, more).await }))
    }

    fn subscribe(&self, channel_id: String, subscribed: bool) -> Option<BoxFuture<()>> {
        let account = self.account(Role::Recommendations);
        account.method()?;
        let recent = self.subscriptions.clone();
        Some(Box::pin(async move {
            channel::subscribe(&account, channel_id.clone(), subscribed).await?;
            recent.set(channel_id, subscribed);
            Ok(())
        }))
    }

    fn search(&self, query: String) -> Option<BoxFuture<SearchPage>> {
        let client = self.client.clone();
        Some(Box::pin(async move {
            let result = client.query().search::<VideoItem, _>(&query).await?;
            Ok(search_page(result.items.items, result.corrected_query, result.items.ctoken, result.visitor_data))
        }))
    }

    fn search_more(&self, more: SearchMore) -> Option<BoxFuture<SearchPage>> {
        let client = self.client.clone();
        Some(Box::pin(async move {
            let page = client
                .query()
                .continuation::<VideoItem, _>(more.token, ContinuationEndpoint::Search, more.visitor_data.as_deref())
                .await?;
            Ok(search_page(page.items, None, page.ctoken, page.visitor_data))
        }))
    }

    fn search_suggestions(&self, query: String) -> Option<BoxFuture<Vec<String>>> {
        let client = self.client.clone();
        Some(Box::pin(async move { Ok(client.query().search_suggestion(&query).await?) }))
    }

    fn web_url(&self, id: &str) -> String {
        format!("https://www.youtube.com/watch?v={id}")
    }

    fn thumbnail_url(&self, id: &str) -> Option<String> {
        Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"))
    }
}

/// Give each audio track's streams their own ids: every dub of a format has the same itag, so
/// the id is the itag plus, in the bits above it, the track's position among the video's tracks.
fn number_tracks(catalog: &mut StreamCatalog) {
    let mut tracks: Vec<String> = Vec::new();
    for stream in &mut catalog.audio {
        let Some(track) = &stream.track else { continue };
        let index = tracks.iter().position(|known| *known == track.id).unwrap_or_else(|| {
            tracks.push(track.id.clone());
            tracks.len() - 1
        });
        stream.id = StreamId(stream.id.0 | (index as u64) << 32);
    }
}

/// The itag of a stream (see [`number_tracks`]).
fn itag(id: StreamId) -> u64 {
    id.0 & 0xffff_ffff
}

/// Have yt-dlp resolve `format` of a video (signed in with a session's `Cookie` header, if
/// given), and return its catalog once the URLs in it work.
async fn resolve_with_ytdlp(id: String, format: String, cookies: Option<String>) -> Result<StreamCatalog> {
    let resolved = tokio::task::spawn_blocking(move || {
        let cookie_file = cookies.as_deref().map(ytdlp::CookieFile::new).transpose()?;
        ytdlp::resolve(&id, &format, cookie_file.as_ref().map(ytdlp::CookieFile::path))
    })
    .await??;
    let wait = resolved.usable_in();
    if !wait.is_zero() {
        eprintln!("moth: YouTube serves yt-dlp's URLs in {:.1}s; holding them until then", wait.as_secs_f64());
        tokio::time::sleep(wait).await;
    }
    Ok(resolved.catalog)
}

fn catalog(player: &VideoPlayer) -> StreamCatalog {
    let video = player
        .video_only_streams
        .iter()
        .filter(|stream| !stream.hdr && stream.drm_track_type.is_none())
        .filter_map(|stream| {
            let codec = match stream.codec {
                RpVideoCodec::Vp9 => VideoCodec::Vp9,
                RpVideoCodec::Av01 => VideoCodec::Av1,
                RpVideoCodec::Avc1 => VideoCodec::H264,
                _ => return None,
            };
            Some(VideoStream {
                id: StreamId(stream.itag.into()),
                location: Location::new(&stream.url),
                quality: stream.quality.clone(),
                width: stream.width,
                height: stream.height,
                fps: stream.fps,
                codec,
                bitrate: Some(stream.average_bitrate),
                size: stream.size,
                premium: false,
            })
        })
        .collect();
    // The iOS client also lists "stable volume" (DRC) re-encodes under the same itags as the
    // regular files. They must be skipped, not just deprioritized: alternate URLs are requested
    // by itag and would serve the regular file, so switching mid-stream would corrupt playback.
    let audio = player
        .audio_streams
        .iter()
        .filter(|stream| !stream.url.contains("drc%3D1") && stream.drm_track_type.is_none())
        .filter_map(|stream| {
            let codec = match stream.codec {
                RpAudioCodec::Opus => AudioCodec::Opus,
                RpAudioCodec::Mp4a => AudioCodec::Aac,
                _ => return None,
            };
            Some(AudioStream {
                id: StreamId(stream.itag.into()),
                location: Location::new(&stream.url),
                codec,
                bitrate: Some(stream.average_bitrate),
                size: Some(stream.size),
                track: stream.track.as_ref().map(|track| AudioTrack {
                    id: track.id.clone(),
                    name: track.lang_name.clone(),
                    is_default: track.is_default,
                }),
            })
        })
        .collect();
    let client = format!("{:?}", player.client_type).to_lowercase();
    StreamCatalog {
        video,
        audio,
        resolver: "rustypipe",
        resolver_detail: format!("RustyPipe ({client} client)"),
        has_alternates: true,
    }
}

fn search_page(
    items: Vec<VideoItem>,
    corrected_query: Option<String>,
    ctoken: Option<String>,
    visitor_data: Option<String>,
) -> SearchPage {
    SearchPage {
        videos: items.into_iter().map(summary).collect(),
        corrected_query,
        more: ctoken.map(|token| SearchMore { source: ID, token, visitor_data }),
    }
}

fn summary(item: VideoItem) -> VideoSummary {
    let channel_avatar_url = item.channel.as_ref().and_then(|channel| pick_thumbnail(&channel.avatar, 48));
    VideoSummary {
        thumbnail_url: pick_thumbnail(&item.thumbnail, 336),
        channel_id: item.channel.as_ref().map(|channel| channel.id.clone()),
        channel: item.channel.map(|channel| channel.name),
        channel_avatar_url,
        duration: item.duration.map(|seconds| Duration::from_secs(seconds.into())),
        views: item.view_count,
        age: item.publish_date_txt,
        description: item.short_description,
        is_short: item.is_short,
        video: VideoRef { source: ID, id: item.id },
        title: item.name,
    }
}

/// Smallest thumbnail at least `min_width` wide (sharp at the UI's size on a 2x display), else
/// the largest.
fn pick_thumbnail(thumbnails: &[Thumbnail], min_width: u32) -> Option<String> {
    thumbnails
        .iter()
        .filter(|thumbnail| thumbnail.width >= min_width)
        .min_by_key(|thumbnail| thumbnail.width)
        .or_else(|| thumbnails.iter().max_by_key(|thumbnail| thumbnail.width))
        .map(|thumbnail| thumbnail.url.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_youtube_links_and_ids() {
        assert!(claims_url("https://www.youtube.com/watch?v=aqz-KE-bpKQ"));
        assert!(claims_url("https://youtu.be/aqz-KE-bpKQ"));
        assert!(!claims_url("https://www.youtube.com.evil.example/watch"));
        assert!(!claims_url("https://vimeo.com/1"));
        assert!(claims_host_path("youtube.com/watch?v=aqz-KE-bpKQ"));
        assert!(claims_host_path("youtu.be/aqz-KE-bpKQ"));
        assert!(!claims_host_path("youtube tutorial"));
        assert!(is_video_id("aqz-KE-bpKQ"));
        assert!(is_video_id("eRsGyueVLvQ"));
        assert!(is_video_id("dQw4w9WgXcQ"));
        assert!(!is_video_id("programming"));
        assert!(!is_video_id("PROGRAMMING"));
        assert!(!is_video_id("PlayStation"));
        assert!(!is_video_id("not an id"));
    }

    #[test]
    fn premium_audio_joins_the_only_track() {
        let mut catalog = StreamCatalog {
            video: Vec::new(),
            audio: vec![audio(251, Some("en"))],
            resolver: "test",
            resolver_detail: String::new(),
            has_alternates: false,
        };
        let base = catalog.clone();
        let premium = |itag| StreamCatalog { audio: vec![audio(itag, None)], ..base.clone() };
        add_premium(&mut catalog, premium(774));
        add_premium(&mut catalog, premium(774));
        assert_eq!(catalog.audio.len(), 2);
        assert_eq!(catalog.audio[1].track.as_ref().map(|track| track.id.as_str()), Some("en"));
        // Several tracks (dubs): the premium audio can't tell which one it is.
        catalog.audio.push(audio(251, Some("fr")));
        add_premium(&mut catalog, premium(141));
        assert_eq!(catalog.audio.len(), 3);
    }

    fn audio(itag: u64, track: Option<&str>) -> AudioStream {
        AudioStream {
            id: StreamId(itag),
            location: Location::new(format!("https://a/{itag}/{}", track.unwrap_or("-"))),
            codec: AudioCodec::Opus,
            bitrate: None,
            size: None,
            track: track.map(|id| AudioTrack { id: id.to_string(), name: id.to_string(), is_default: id == "en.4" }),
        }
    }

    #[test]
    fn dubs_get_their_own_ids() {
        let mut catalog = StreamCatalog {
            video: Vec::new(),
            audio: vec![
                audio(251, Some("en.4")),
                audio(140, Some("en.4")),
                audio(251, Some("de.3")),
                audio(140, Some("de.3")),
            ],
            resolver: "test",
            resolver_detail: String::new(),
            has_alternates: false,
        };
        number_tracks(&mut catalog);
        let ids: Vec<_> = catalog.audio.iter().map(|audio| audio.id).collect();
        assert_eq!(ids, [StreamId(251), StreamId(140), StreamId(251 | 1 << 32), StreamId(140 | 1 << 32)]);
        assert_eq!(catalog.audio_stream(ids[2]).map(|audio| audio.location.url.as_str()), Some("https://a/251/de.3"));
        assert_eq!(itag(ids[3]), 140);
        // No tracks: ids stay itags.
        let mut single = StreamCatalog { audio: vec![audio(251, None)], ..catalog };
        number_tracks(&mut single);
        assert_eq!(single.audio[0].id, StreamId(251));
    }

    /// Network test: `cargo test -- --ignored --nocapture live_search`
    #[test]
    #[ignore]
    fn live_search() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let youtube =
            runtime.block_on(YouTube::new(reqwest::Client::new(), Resolver::default(), None)).expect("client");

        let suggestions =
            runtime.block_on(youtube.search_suggestions("blender spring".into()).unwrap()).expect("suggest");
        println!("== YouTube suggestions: {suggestions:?}");
        assert!(!suggestions.is_empty());

        let page = runtime.block_on(youtube.search("blender spring".into()).unwrap()).expect("search");
        println!("== YouTube search: {} videos, more: {}", page.videos.len(), page.more.is_some());
        for video in page.videos.iter().take(3) {
            println!("  {} | {:?} | {:?} | {:?}", video.title, video.channel, video.duration, video.description);
        }
        assert!(!page.videos.is_empty());
        let more = runtime.block_on(youtube.search_more(page.more.expect("more")).unwrap()).expect("search more");
        println!("== YouTube search page 2: {} videos", more.videos.len());
        assert!(!more.videos.is_empty());

        let music_suggest =
            runtime.block_on(youtube.music_search_suggestions("daft punk".into()).unwrap()).expect("music suggest");
        println!(
            "== Music suggestions: {} terms {:?}, {} items",
            music_suggest.terms.len(),
            music_suggest.terms,
            music_suggest.items.len()
        );
        for item in &music_suggest.items {
            println!("  suggest item: {:?} | {} | {} | play={:?}", item.kind, item.title, item.subtitle, item.play);
        }
        assert!(!music_suggest.terms.is_empty());

        let music_page = runtime
            .block_on(youtube.music_search("daft punk".into(), MusicSearchFilter::All).unwrap())
            .expect("music search");
        println!("== Music search (All): {} shelves, more={}", music_page.shelves.len(), music_page.more.is_some());
        for shelf in &music_page.shelves {
            println!("  shelf {:?} (filter={:?}): {} items", shelf.title, shelf.filter, shelf.items.len());
            for item in shelf.items.iter().take(3) {
                println!("    {:?} | {} | {} | play={:?}", item.kind, item.title, item.subtitle, item.play);
            }
        }
        assert!(music_page.shelves.iter().map(|s| s.items.len()).sum::<usize>() > 5);

        for filter in [
            MusicSearchFilter::Songs,
            MusicSearchFilter::Videos,
            MusicSearchFilter::Albums,
            MusicSearchFilter::Playlists,
            MusicSearchFilter::Artists,
        ] {
            let page = runtime
                .block_on(youtube.music_search("daft punk".into(), filter).unwrap())
                .expect("filtered music search");
            let count = page.shelves.first().map_or(0, |s| s.items.len());
            println!("== Music search ({:?}): {} items, more={}", filter, count, page.more.is_some());
            assert!(count > 0);
            if filter == MusicSearchFilter::Songs {
                let more = runtime
                    .block_on(youtube.music_search_more(page.more.expect("songs more")).unwrap())
                    .expect("music songs more");
                println!("== Music search (Songs page 2): {} items", more.shelves.first().map_or(0, |s| s.items.len()));
                assert!(!more.shelves.is_empty());
            }
        }
    }

    /// Network test: `cargo test -- --ignored --nocapture live_channel`
    #[test]
    #[ignore]
    fn live_channel() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let youtube =
            runtime.block_on(YouTube::new(reqwest::Client::new(), Resolver::default(), None)).expect("client");
        let id = runtime.block_on(youtube.resolve_channel("@veritasium".into()).unwrap()).expect("resolve");
        println!("== @veritasium is {id}");
        let channel = runtime.block_on(youtube.channel(id.clone()).unwrap()).expect("channel");
        println!(
            "== {} {:?} subs {:?} videos {:?} verified {} subscribed {:?}",
            channel.name,
            channel.handle,
            channel.subscribers,
            channel.video_count,
            channel.verified,
            channel.subscribed
        );
        println!("   avatar {:?}\n   banner {:?}", channel.avatar_url, channel.banner_url);
        println!("   {} videos, more: {}", channel.videos.videos.len(), channel.videos.more.is_some());
        for video in channel.videos.videos.iter().take(3) {
            println!("   {} | {:?} | {:?} | {:?}", video.title, video.channel, video.channel_id, video.views);
        }
        assert!(!channel.videos.videos.is_empty());
        let more =
            runtime.block_on(youtube.channel_videos_more(channel.videos.more.expect("more")).unwrap()).expect("more");
        println!("== page 2: {} videos", more.videos.len());
        assert!(!more.videos.is_empty());
        let popular = runtime.block_on(youtube.channel_videos(id, ChannelOrder::Popular).unwrap()).expect("popular");
        println!(
            "== popular: {} videos, first {:?} {:?}",
            popular.videos.len(),
            popular.videos[0].title,
            popular.videos[0].views
        );
        assert!(!popular.videos.is_empty());
    }

    /// Network test that subscribes the recommendation account to a channel and unsubscribes
    /// it again: `cargo test -- --ignored --nocapture live_subscribe`
    #[test]
    #[ignore]
    fn live_subscribe() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let youtube =
            runtime.block_on(YouTube::new(reqwest::Client::new(), Resolver::default(), None)).expect("client");
        let id = "UCHnyfMqiRRG1u-2MsSQLbXA".to_string();
        let state =
            |youtube: &YouTube| runtime.block_on(youtube.channel(id.clone()).unwrap()).expect("channel").subscribed;
        let before = state(&youtube);
        println!("before: {before:?}");
        assert_eq!(before, Some(false), "expected a channel the account isn't subscribed to");
        runtime.block_on(youtube.subscribe(id.clone(), true).unwrap()).expect("subscribe");
        let mut during = state(&youtube);
        for _ in 0..4 {
            println!("after subscribing: {during:?}");
            if during == Some(true) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
            during = state(&youtube);
        }
        runtime.block_on(youtube.subscribe(id.clone(), false).unwrap()).expect("unsubscribe");
        // The state a page reports lags a few seconds behind.
        let mut after = state(&youtube);
        for _ in 0..4 {
            println!("after unsubscribing: {after:?}");
            if after == Some(false) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
            after = state(&youtube);
        }
        assert_eq!((during, after), (Some(true), Some(false)));
    }

    /// Network test: `cargo test -- --ignored --nocapture live_music`
    #[test]
    #[ignore]
    fn live_music() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let youtube =
            runtime.block_on(YouTube::new(reqwest::Client::new(), Resolver::default(), None)).expect("client");
        let shelves = runtime.block_on(youtube.music_home().unwrap()).expect("music home");
        for shelf in &shelves {
            println!(
                "== {} ({:?}): {} items, e.g. {:?}",
                shelf.title,
                shelf.layout,
                shelf.items.len(),
                shelf.items[0].title
            );
        }
        let target =
            shelves.iter().flat_map(|shelf| &shelf.items).find_map(|item| item.play.clone()).expect("a target");
        let page = runtime.block_on(youtube.music_queue(target.clone()).unwrap()).expect("queue");
        println!("queue for {target:?}: {} tracks, more: {}", page.tracks.len(), page.more.is_some());
        for track in page.tracks.iter().take(3) {
            println!("  {} | {} | {:?} | {:?}", track.title, track.artists, track.detail, track.duration);
        }
        let more = runtime.block_on(youtube.music_queue_more(page.more.expect("more")).unwrap()).expect("more");
        println!("next page: {} tracks", more.tracks.len());
        assert!(!more.tracks.is_empty());
    }

    /// Network test: `cargo test -- --ignored --nocapture live_catalog`
    #[test]
    #[ignore]
    fn live_catalog() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let preferences = StreamPreferences::default();
        for (first, id) in [
            (Resolver::VisionOs, "WhWc3b3KhnY"),
            (Resolver::VisionOs, "0e3GPea1Tyg"),
            (Resolver::RustyPipe, "0e3GPea1Tyg"),
        ] {
            let youtube = runtime.block_on(YouTube::new(reqwest::Client::new(), first, None)).expect("client");
            let catalog = runtime.block_on(youtube.streams(id.to_string(), preferences.clone())).expect("streams");
            println!("== {id} via {}: qualities {:?}", catalog.resolver_detail, catalog.qualities());
            let video = catalog.auto_video(&preferences, None).expect("auto video");
            assert!(video.resolution() <= preferences.max_height);
            let audio = catalog.audio_in_track(None, &preferences);
            assert_eq!(audio.first().map(|a| a.codec), Some(AudioCodec::Opus));
            let mut ids: Vec<_> = catalog.audio.iter().map(|audio| audio.id).collect();
            ids.sort_by_key(|id| id.0);
            ids.dedup();
            assert_eq!(ids.len(), catalog.audio.len(), "audio ids must be unique");
            for track in catalog.tracks().iter().filter(|track| !track.is_default).take(1) {
                let streams = catalog.audio_in_track(Some(&track.id), &preferences);
                assert!(streams.iter().all(|a| a.track.as_ref().map(|t| &t.id) == Some(&track.id)));
            }
        }
    }
}
