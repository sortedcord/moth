//! YouTube Music: the music home's shelves, play queues ("up next", radio), and likes, through
//! the web app's InnerTube client (`WEB_REMIX`, RustyPipe's `DesktopMusic`).
//!
//! The home is the recommendation account's when it's signed in with a browser session; YouTube
//! Music refuses the TV app's token, so with a code sign-in (or none) it's the signed-out home.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use rustypipe::client::{ClientType, RustyPipe, RustyPipeQuery};
use serde_json::Value;

use super::account::Account;
use super::feed::{pick_thumbnail, text, thumbnails};
use crate::backend::accounts::Method;
use crate::backend::media::VideoRef;
use crate::backend::music::{MusicItem, MusicKind, MusicShelf, PlayTarget, QueuePage, Rating, ShelfLayout, Track};

/// Requests for YouTube Music: the account's if it has a browser session, else signed out.
fn music_query(account: &Account, signed_out: &RustyPipe) -> RustyPipeQuery {
    match account.query() {
        Some((query, _)) if account.method() == Some(Method::Cookies) => query,
        _ => signed_out.query().unauthenticated(),
    }
}

async fn request(query: &RustyPipeQuery, endpoint: &str, body: Value) -> Result<Value> {
    let response = query.raw(ClientType::DesktopMusic, endpoint, &body).await?;
    serde_json::from_str(&response).with_context(|| format!("unexpected {endpoint} response"))
}

pub async fn home(account: &Account, signed_out: &RustyPipe) -> Result<Vec<MusicShelf>> {
    let query = music_query(account, signed_out);
    let response = request(&query, "browse", serde_json::json!({"browseId": "FEmusic_home"})).await?;
    let shelves = shelves(&response);
    if shelves.is_empty() {
        bail!("the music home is empty");
    }
    Ok(shelves)
}

/// The queue `target` starts: its playlist, or the track's radio.
pub async fn queue(account: &Account, signed_out: &RustyPipe, target: &PlayTarget) -> Result<QueuePage> {
    let query = music_query(account, signed_out);
    let playlist = target.playlist.clone().or_else(|| target.track.as_ref().map(|track| format!("RDAMVM{track}")));
    let mut body = serde_json::json!({
        "isAudioOnly": true,
        "enablePersistentPlaylistPanel": true,
        "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
    });
    if let Some(track) = &target.track {
        body["videoId"] = track.clone().into();
    }
    if let Some(playlist) = playlist {
        body["playlistId"] = playlist.into();
    }
    let page = queue_page(&request(&query, "next", body).await?);
    if page.tracks.is_empty() {
        bail!("nothing to play there");
    }
    Ok(page)
}

/// The next page of a queue (see [`QueuePage::more`]).
pub async fn queue_more(account: &Account, signed_out: &RustyPipe, more: &str) -> Result<QueuePage> {
    let query = music_query(account, signed_out);
    let body = serde_json::json!({"continuation": more, "isAudioOnly": true, "enablePersistentPlaylistPanel": true});
    Ok(queue_page(&request(&query, "next", body).await?))
}

/// The signed-in account's rating of a track.
pub async fn rating(account: &Account, id: &str) -> Result<Rating> {
    let (query, client) = rating_query(account)?;
    let body = serde_json::json!({"videoId": id, "isAudioOnly": true});
    let response = query.raw(client, "next", &body).await?;
    let response: Value = serde_json::from_str(&response).context("unexpected next response")?;
    Ok(like_status(&response, id))
}

pub async fn rate(account: &Account, id: &str, rating: Rating) -> Result<()> {
    let (query, client) = rating_query(account)?;
    let endpoint = match rating {
        Rating::Like => "like/like",
        Rating::Dislike => "like/dislike",
        Rating::None => "like/removelike",
    };
    query.raw(client, endpoint, &serde_json::json!({"target": {"videoId": id}})).await?;
    Ok(())
}

/// Ratings go to the account whichever way it's signed in: the web app with a session, the TV
/// app with a code.
fn rating_query(account: &Account) -> Result<(RustyPipeQuery, ClientType)> {
    match account.query() {
        Some((query, _)) if account.method() == Some(Method::Cookies) => Ok((query, ClientType::DesktopMusic)),
        Some((query, client)) => Ok((query, client)),
        None => bail!("not signed in"),
    }
}

// ── Parsing ──────────────────────────────────────────────────────────────────

/// Every object under `key` anywhere in `value`, in order.
fn find_all<'a>(value: &'a Value, key: &str, found: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            for (name, inner) in map {
                if name == key {
                    found.push(inner);
                } else {
                    find_all(inner, key, found);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| find_all(item, key, found)),
        _ => {}
    }
}

fn find_first<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    let mut found = Vec::new();
    find_all(value, key, &mut found);
    found.into_iter().next()
}

/// The carousels of a music browse response (home, continuations).
fn shelves(response: &Value) -> Vec<MusicShelf> {
    let mut carousels = Vec::new();
    find_all(response, "musicCarouselShelfRenderer", &mut carousels);
    find_all(response, "musicImmersiveCarouselShelfRenderer", &mut carousels);
    carousels
        .into_iter()
        .filter_map(|shelf| {
            let title = shelf
                .pointer("/header/musicCarouselShelfBasicHeaderRenderer/title")
                .or_else(|| shelf.pointer("/header/musicImmersiveCarouselShelfBasicHeaderRenderer/title"))
                .and_then(text)?;
            let contents = shelf.get("contents")?.as_array()?;
            let list = contents.iter().any(|item| item.get("musicResponsiveListItemRenderer").is_some());
            let items: Vec<MusicItem> = contents
                .iter()
                .filter_map(|item| {
                    item.get("musicTwoRowItemRenderer")
                        .and_then(two_row_item)
                        .or_else(|| item.get("musicResponsiveListItemRenderer").and_then(list_item))
                })
                .collect();
            let layout = if list { ShelfLayout::List } else { ShelfLayout::Cards };
            (!items.is_empty()).then_some(MusicShelf { title, layout, items })
        })
        .collect()
}

/// What playing an item starts: its play button, else where clicking it goes, else its radio.
fn play_target(item: &Value) -> Option<PlayTarget> {
    let endpoint = |value: Option<&Value>| -> Option<PlayTarget> {
        let value = value?;
        let watch = find_first(value, "watchEndpoint").or_else(|| find_first(value, "watchPlaylistEndpoint"))?;
        let field = |name: &str| watch.get(name).and_then(Value::as_str).map(str::to_string);
        let (track, playlist) = (field("videoId"), field("playlistId"));
        (track.is_some() || playlist.is_some()).then_some(PlayTarget { source: super::ID, track, playlist })
    };
    let radio = item.pointer("/menu/menuRenderer/items").and_then(Value::as_array).and_then(|items| {
        items.iter().find(|entry| {
            entry.pointer("/menuNavigationItemRenderer/icon/iconType").and_then(Value::as_str) == Some("MIX")
        })
    });
    endpoint(item.get("thumbnailOverlay").or_else(|| item.get("overlay")))
        .or_else(|| endpoint(item.get("navigationEndpoint")))
        .or_else(|| endpoint(radio))
}

/// Song or video, from the watch endpoint's music video type.
fn watch_kind(item: &Value) -> Option<MusicKind> {
    let video_type = find_first(item, "musicVideoType")?.as_str()?;
    Some(if video_type == "MUSIC_VIDEO_TYPE_ATV" { MusicKind::Song } else { MusicKind::Video })
}

fn page_kind(item: &Value) -> Option<MusicKind> {
    let page_type = find_first(item.get("navigationEndpoint")?, "pageType")?.as_str()?;
    Some(match page_type {
        "MUSIC_PAGE_TYPE_ALBUM" | "MUSIC_PAGE_TYPE_AUDIOBOOK" => MusicKind::Album,
        "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL" => MusicKind::Artist,
        _ => MusicKind::Playlist,
    })
}

/// A card: album, playlist, artist, or a song/video.
fn two_row_item(item: &Value) -> Option<MusicItem> {
    let kind = page_kind(item).or_else(|| watch_kind(item)).unwrap_or(MusicKind::Playlist);
    Some(MusicItem {
        kind,
        title: item.get("title").and_then(text)?,
        subtitle: item.get("subtitle").and_then(text).unwrap_or_default(),
        thumbnail_url: pick_thumbnail(
            &thumbnails(item.pointer("/thumbnailRenderer/musicThumbnailRenderer/thumbnail/thumbnails")),
            226,
        ),
        play: play_target(item),
    })
}

/// A song row: the title, then columns of artists, album, plays.
fn list_item(item: &Value) -> Option<MusicItem> {
    let columns: Vec<String> = item
        .get("flexColumns")?
        .as_array()?
        .iter()
        .filter_map(|column| column.pointer("/musicResponsiveListItemFlexColumnRenderer/text").and_then(text))
        .collect();
    let (title, rest) = columns.split_first()?;
    Some(MusicItem {
        kind: watch_kind(item).unwrap_or(MusicKind::Song),
        title: title.clone(),
        subtitle: rest.join(" • "),
        thumbnail_url: pick_thumbnail(
            &thumbnails(item.pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")),
            96,
        ),
        play: play_target(item),
    })
}

/// A `next` response's queue: its tracks (the primary of each song/video pair) and the
/// continuation for more.
fn queue_page(response: &Value) -> QueuePage {
    let mut panels = Vec::new();
    find_all(response, "playlistPanelVideoRenderer", &mut panels);
    // A song/video pair lists the other one as `counterpart`; that one isn't a queue entry.
    let mut counterparts = Vec::new();
    find_all(response, "counterpart", &mut counterparts);
    let mut skip = Vec::new();
    for counterpart in counterparts {
        find_all(counterpart, "playlistPanelVideoRenderer", &mut skip);
    }
    let tracks = panels
        .into_iter()
        .filter(|panel| !skip.iter().any(|other| std::ptr::eq(*other, *panel)))
        .filter_map(track)
        .collect();
    let more = ["nextRadioContinuationData", "nextContinuationData"]
        .iter()
        .find_map(|key| find_first(response, key)?.get("continuation")?.as_str().map(str::to_string));
    QueuePage { tracks, more }
}

fn track(panel: &Value) -> Option<Track> {
    let id = panel.get("videoId")?.as_str()?;
    let byline = panel.get("longBylineText").and_then(text).unwrap_or_default();
    let mut parts = byline.split(" • ").map(str::trim).filter(|part| !part.is_empty());
    let artists = panel.get("shortBylineText").and_then(text).or_else(|| parts.next().map(str::to_string));
    let detail: Vec<&str> = byline.split(" • ").map(str::trim).skip(1).filter(|part| !part.is_empty()).collect();
    Some(Track {
        video: VideoRef { source: super::ID, id: id.to_string() },
        title: panel.get("title").and_then(text)?,
        artists: artists.unwrap_or_default(),
        detail: (!detail.is_empty()).then(|| detail.join(" • ")),
        duration: panel.get("lengthText").and_then(text).and_then(|length| clock(&length)),
        thumbnail_url: pick_thumbnail(&thumbnails(panel.pointer("/thumbnail/thumbnails")), 120),
        artwork_url: pick_thumbnail(&thumbnails(panel.pointer("/thumbnail/thumbnails")), u32::MAX)
            .map(|url| artwork(&url)),
    })
}

fn like_status(response: &Value, id: &str) -> Rating {
    let mut buttons = Vec::new();
    find_all(response, "likeButtonRenderer", &mut buttons);
    let button =
        buttons.into_iter().find(|button| button.pointer("/target/videoId").and_then(Value::as_str) == Some(id));
    match button.and_then(|button| button.get("likeStatus")).and_then(Value::as_str) {
        Some("LIKE") => Rating::Like,
        Some("DISLIKE") => Rating::Dislike,
        _ => Rating::None,
    }
}

/// A thumbnail URL at artwork size: googleusercontent images take their size from the URL
/// (`=w226-h226-…`), so ask for a big one; others (i.ytimg.com) stay as they are.
fn artwork(url: &str) -> String {
    match url.rsplit_once("=w") {
        Some((base, params)) if url.contains("googleusercontent.com") => {
            let rest = params.split_once('-').and_then(|(_, rest)| rest.split_once('-')).map_or("", |(_, rest)| rest);
            if rest.is_empty() { format!("{base}=w720-h720") } else { format!("{base}=w720-h720-{rest}") }
        }
        _ => url.to_string(),
    }
}

/// "4:29" → 269 s.
fn clock(text: &str) -> Option<Duration> {
    let mut seconds = 0;
    for part in text.trim().split(':') {
        seconds = seconds * 60 + part.parse::<u64>().ok()?;
    }
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch(id: &str, video_type: &str) -> Value {
        serde_json::json!({"watchEndpoint": {"videoId": id, "playlistId": format!("RDAMVM{id}"),
            "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {"musicVideoType": video_type}}}})
    }

    #[test]
    fn reads_card_and_list_shelves() {
        let response = serde_json::json!({"contents": {"sectionListRenderer": {"contents": [
            {"musicCarouselShelfRenderer": {
                "header": {"musicCarouselShelfBasicHeaderRenderer": {"title": {"runs": [{"text": "Hindi Hits"}]}}},
                "contents": [{"musicTwoRowItemRenderer": {
                    "title": {"runs": [{"text": "Bollywood Romantic Moments"}]},
                    "subtitle": {"runs": [{"text": "Arijit Singh"}, {"text": ", "}, {"text": "Pritam"}]},
                    "thumbnailRenderer": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [
                        {"url": "https://t/226", "width": 226}, {"url": "https://t/544", "width": 544}]}}},
                    "navigationEndpoint": {"browseEndpoint": {"browseId": "VLRDCLAK5uy", "browseEndpointContextSupportedConfigs":
                        {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_PLAYLIST"}}}},
                    "thumbnailOverlay": {"musicItemThumbnailOverlayRenderer": {"content": {"musicPlayButtonRenderer": {
                        "playNavigationEndpoint": {"watchPlaylistEndpoint": {"playlistId": "RDCLAK5uy"}}}}}}}}]}},
            {"musicCarouselShelfRenderer": {
                "header": {"musicCarouselShelfBasicHeaderRenderer": {"title": {"runs": [{"text": "Song mixes"}]}}},
                "contents": [{"musicResponsiveListItemRenderer": {
                    "flexColumns": [
                        {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Todo Lo Fue"}]}}},
                        {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Natalia Lafourcade"}]}}},
                        {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "12M plays"}]}}}],
                    "overlay": {"musicItemThumbnailOverlayRenderer": {"content": {"musicPlayButtonRenderer": {
                        "playNavigationEndpoint": watch("tdS3ijuqxW4", "MUSIC_VIDEO_TYPE_ATV")}}}}}}]}}
        ]}}});
        let shelves = shelves(&response);
        assert_eq!(shelves.len(), 2);
        let card = &shelves[0].items[0];
        assert_eq!((shelves[0].layout, card.kind), (ShelfLayout::Cards, MusicKind::Playlist));
        assert_eq!(
            (card.subtitle.as_str(), card.thumbnail_url.as_deref()),
            ("Arijit Singh, Pritam", Some("https://t/226"))
        );
        assert_eq!(card.play.as_ref().and_then(|play| play.playlist.as_deref()), Some("RDCLAK5uy"));
        let song = &shelves[1].items[0];
        assert_eq!((shelves[1].layout, song.kind), (ShelfLayout::List, MusicKind::Song));
        assert_eq!(song.subtitle, "Natalia Lafourcade • 12M plays");
        assert_eq!(song.play.as_ref().and_then(|play| play.track.as_deref()), Some("tdS3ijuqxW4"));
    }

    #[test]
    fn reads_queue_without_counterparts() {
        let panel = |id: &str, title: &str| {
            serde_json::json!({"playlistPanelVideoRenderer": {"videoId": id, "title": {"runs": [{"text": title}]},
                "shortBylineText": {"runs": [{"text": "Rita"}]},
                "longBylineText": {"runs": [{"text": "Rita"}, {"text": " • "}, {"text": "Little Busters! OST"}, {"text": " • "}, {"text": "2007"}]},
                "lengthText": {"runs": [{"text": "5:14"}]}}})
        };
        let response = serde_json::json!({"playlistPanelRenderer": {"contents": [
            panel("aaaaaaaaaaa", "Song for friends"),
            {"playlistPanelVideoWrapperRenderer": {"primaryRenderer": panel("bbbbbbbbbbb", "Song"),
                "counterpart": [{"counterpartRenderer": panel("ccccccccccc", "Its video")}]}}],
            "continuations": [{"nextRadioContinuationData": {"continuation": "more-please"}}]}});
        let page = queue_page(&response);
        let ids: Vec<&str> = page.tracks.iter().map(|track| track.video.id.as_str()).collect();
        assert_eq!(ids, ["aaaaaaaaaaa", "bbbbbbbbbbb"]);
        let first = &page.tracks[0];
        assert_eq!((first.artists.as_str(), first.detail.as_deref()), ("Rita", Some("Little Busters! OST • 2007")));
        assert_eq!(first.duration, Some(Duration::from_secs(314)));
        assert_eq!(page.more.as_deref(), Some("more-please"));
    }

    #[test]
    fn asks_for_big_artwork() {
        assert_eq!(
            artwork("https://lh3.googleusercontent.com/abc=w120-h120-l90-rj"),
            "https://lh3.googleusercontent.com/abc=w720-h720-l90-rj"
        );
        assert_eq!(
            artwork("https://lh3.googleusercontent.com/abc=w60-h60"),
            "https://lh3.googleusercontent.com/abc=w720-h720"
        );
        assert_eq!(artwork("https://i.ytimg.com/vi/x/hqdefault.jpg"), "https://i.ytimg.com/vi/x/hqdefault.jpg");
    }

    #[test]
    fn reads_like_status() {
        let response = serde_json::json!({"playerOverlays": {"playerOverlayRenderer": {"actions": [
            {"likeButtonRenderer": {"target": {"videoId": "aaaaaaaaaaa"}, "likeStatus": "LIKE"}}]}}});
        assert_eq!(like_status(&response, "aaaaaaaaaaa"), Rating::Like);
        assert_eq!(like_status(&response, "bbbbbbbbbbb"), Rating::None);
    }
}
