//! YouTube Music: the music home's shelves, play queues ("up next", radio), and likes, through
//! the web app's InnerTube client (`WEB_REMIX`, RustyPipe's `DesktopMusic`).
//!
//! The home is the recommendation account's: YouTube Music's own with a browser session, or,
//! since YouTube Music refuses the TV app's token, the TV app's music page (`FEtopics_music`:
//! "Recently played", "Listen again", …) with a code sign-in. Signed out, it's YouTube Music's
//! signed-out home.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use rustypipe::client::{ClientType, RustyPipe, RustyPipeQuery};
use serde_json::Value;

use super::account::Account;
use super::feed::{pick_thumbnail, text, thumbnails};
use crate::backend::accounts::Method;
use crate::backend::media::VideoRef;
use crate::backend::music::{
    MusicItem, MusicKind, MusicSearchFilter, MusicSearchMore, MusicSearchPage, MusicSearchShelf,
    MusicSearchSuggestions, MusicShelf, PlayTarget, QueuePage, Rating, ShelfLayout, Track,
};

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

/// Continuation pages of the TV app's music page to load (each adds a few shelves).
const TV_PAGES: usize = 3;

pub async fn home(account: &Account, signed_out: &RustyPipe) -> Result<Vec<MusicShelf>> {
    if account.method() == Some(Method::DeviceCode)
        && let Some((query, client)) = account.query()
    {
        return tv_home(&query, client).await;
    }
    let query = music_query(account, signed_out);
    let response = request(&query, "browse", serde_json::json!({"browseId": "FEmusic_home"})).await?;
    let shelves = shelves(&response);
    if shelves.is_empty() {
        bail!("the music home is empty");
    }
    Ok(shelves)
}

/// The TV app's music page, and a few of its continuations.
async fn tv_home(query: &RustyPipeQuery, client: ClientType) -> Result<Vec<MusicShelf>> {
    let mut body = serde_json::json!({"browseId": "FEtopics_music"});
    let mut shelves = Vec::new();
    for _ in 0..=TV_PAGES {
        let response = query.raw(client, "browse", &body).await?;
        let response: Value = serde_json::from_str(&response).context("unexpected music page")?;
        // Each shelf shows a few tiles and continues with more; fetch those alongside.
        let page = tv_shelves(&response);
        let more = page.iter().map(|(_, more)| async move {
            let more = more.as_deref()?;
            let response = query.raw(client, "browse", &serde_json::json!({"continuation": more})).await.ok()?;
            let response: Value = serde_json::from_str(&response).ok()?;
            let mut tiles = Vec::new();
            find_all(&response, "tileRenderer", &mut tiles);
            Some(tiles.into_iter().filter_map(tv_tile).collect::<Vec<_>>())
        });
        let more = futures::future::join_all(more).await;
        for ((mut shelf, _), more) in page.into_iter().zip(more) {
            shelf.items.extend(more.into_iter().flatten());
            shelves.push(shelf);
        }
        // The page's own continuation is the section list's (each shelf has one of its own too).
        let Some(more) = find_first(&response, "sectionListRenderer")
            .or_else(|| find_first(&response, "sectionListContinuation"))
            .and_then(|list| list.pointer("/continuations/0/nextContinuationData/continuation"))
            .and_then(Value::as_str)
        else {
            break;
        };
        body = serde_json::json!({"continuation": more});
    }
    if shelves.is_empty() {
        bail!("the music page is empty");
    }
    Ok(shelves)
}

/// The queue `target` starts: its playlist, the track's radio, or an album/artist resolved
/// from its browse ID.
pub async fn queue(account: &Account, signed_out: &RustyPipe, target: &PlayTarget) -> Result<QueuePage> {
    let query = music_query(account, signed_out);
    let (mut track, mut playlist) = (target.track.clone(), target.playlist.clone());
    if track.is_none()
        && playlist.is_none()
        && let Some(browse) = &target.browse
    {
        if browse.starts_with("MPREb_") {
            let album = signed_out.query().music_album(browse).await?;
            playlist = album.playlist_id;
            if playlist.is_none() {
                track = album.tracks.first().map(|t| t.id.clone());
            }
        } else if browse.starts_with("UC") {
            let artist = signed_out.query().music_artist(browse, false).await?;
            playlist = artist.radio_id.or(artist.tracks_playlist_id);
            if playlist.is_none() {
                track = artist.tracks.first().map(|t| t.id.clone());
            }
        }
    }
    let playlist = playlist.or_else(|| track.as_ref().map(|track| format!("RDAMVM{track}")));
    let mut body = serde_json::json!({
        "isAudioOnly": true,
        "enablePersistentPlaylistPanel": true,
        "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
    });
    if let Some(track) = &track {
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

fn filter_params(filter: MusicSearchFilter) -> Option<&'static str> {
    match filter {
        MusicSearchFilter::All => None,
        MusicSearchFilter::Songs => Some("EgWKAQIIAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D"),
        MusicSearchFilter::Videos => Some("EgWKAQIQAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D"),
        MusicSearchFilter::Albums => Some("EgWKAQIYAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D"),
        MusicSearchFilter::Playlists => Some("EgeKAQQoAEABahAQAxAEEAkQChAFEBEQEBAV"),
        MusicSearchFilter::Artists => Some("EgWKAQIgAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D"),
    }
}

pub async fn search(
    account: &Account,
    signed_out: &RustyPipe,
    query_str: &str,
    filter: MusicSearchFilter,
) -> Result<MusicSearchPage> {
    let query = music_query(account, signed_out);
    let mut body = serde_json::json!({"query": query_str});
    if let Some(params) = filter_params(filter) {
        body["params"] = params.into();
    }
    let response = request(&query, "search", body).await?;
    Ok(parse_search(&response, filter))
}

pub async fn search_more(account: &Account, signed_out: &RustyPipe, more: &str) -> Result<MusicSearchPage> {
    let query = music_query(account, signed_out);
    let response = request(&query, "search", serde_json::json!({"continuation": more})).await?;
    Ok(parse_search_continuation(&response))
}

pub async fn search_suggestions(
    account: &Account,
    signed_out: &RustyPipe,
    input: &str,
) -> Result<MusicSearchSuggestions> {
    let query = music_query(account, signed_out);
    let response = request(&query, "music/get_search_suggestions", serde_json::json!({"input": input})).await?;
    Ok(parse_search_suggestions(&response))
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

/// The TV app's shelves of tiles (videos, albums, playlists, artists), each with the
/// continuation for more of its tiles.
fn tv_shelves(response: &Value) -> Vec<(MusicShelf, Option<String>)> {
    let mut found = Vec::new();
    find_all(response, "shelfRenderer", &mut found);
    found
        .into_iter()
        .filter_map(|shelf| {
            let header = shelf.pointer("/headerRenderer/shelfHeaderRenderer")?;
            let title = header
                .pointer("/avatarLockup/avatarLockupRenderer/title")
                .or_else(|| header.get("title"))
                .and_then(text)?;
            let mut tiles = Vec::new();
            find_all(shelf.get("content")?, "tileRenderer", &mut tiles);
            let items: Vec<MusicItem> = tiles.into_iter().filter_map(tv_tile).collect();
            let more = shelf
                .pointer("/content/horizontalListRenderer/continuations/0/nextContinuationData/continuation")
                .and_then(Value::as_str)
                .map(str::to_string);
            (!items.is_empty()).then_some((MusicShelf { title, layout: ShelfLayout::Cards, items }, more))
        })
        .collect()
}

fn tv_tile(tile: &Value) -> Option<MusicItem> {
    let metadata = tile.pointer("/metadata/tileMetadataRenderer")?;
    let lines: Vec<String> = metadata
        .get("lines")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|line| line.pointer("/lineRenderer/items").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|item| item.pointer("/lineItemRenderer/text").and_then(text))
        .filter(|part| part.trim() != "•")
        .collect();
    let command = tile.get("onSelectCommand")?;
    let (kind, play) = if let Some(watch) = command.get("watchEndpoint") {
        let field = |name: &str| watch.get(name).and_then(Value::as_str).map(str::to_string);
        // The TV app's radio ids (`RD<id>`) aren't YouTube Music's; the track's own radio is.
        (MusicKind::Video, PlayTarget { source: super::ID, track: field("videoId"), playlist: None, browse: None })
    } else {
        let browse = command.get("browseEndpoint")?;
        let id = browse.get("browseId")?.as_str()?;
        let kind = match find_first(browse, "pageType").and_then(Value::as_str) {
            Some("MUSIC_PAGE_TYPE_ALBUM") => MusicKind::Album,
            Some("MUSIC_PAGE_TYPE_ARTIST") => MusicKind::Artist,
            _ if id.starts_with("UC") => MusicKind::Artist,
            _ => MusicKind::Playlist,
        };
        // Artists have no queue of their own here.
        let playlist = id.strip_prefix("VL").filter(|_| kind != MusicKind::Artist).map(str::to_string);
        (kind, PlayTarget { source: super::ID, track: None, playlist, browse: None })
    };
    let play = (play.track.is_some() || play.playlist.is_some() || play.browse.is_some()).then_some(play);
    Some(MusicItem {
        kind,
        title: metadata.get("title").and_then(text)?,
        subtitle: lines.join(" • "),
        thumbnail_url: pick_thumbnail(
            &thumbnails(tile.pointer("/header/tileHeaderRenderer/thumbnail/thumbnails")),
            226,
        ),
        play,
    })
}

/// What playing an item starts: its play button, else where clicking it goes, else its radio,
/// else a browse ID (album or artist) resolved when played.
fn play_target(item: &Value) -> Option<PlayTarget> {
    let endpoint = |value: Option<&Value>| -> Option<PlayTarget> {
        let value = value?;
        let watch = find_first(value, "watchEndpoint").or_else(|| find_first(value, "watchPlaylistEndpoint"))?;
        let field = |name: &str| watch.get(name).and_then(Value::as_str).map(str::to_string);
        let (track, playlist) = (field("videoId"), field("playlistId"));
        (track.is_some() || playlist.is_some()).then_some(PlayTarget {
            source: super::ID,
            track,
            playlist,
            browse: None,
        })
    };
    let radio = item.pointer("/menu/menuRenderer/items").and_then(Value::as_array).and_then(|items| {
        items.iter().find(|entry| {
            entry.pointer("/menuNavigationItemRenderer/icon/iconType").and_then(Value::as_str) == Some("MIX")
        })
    });
    let browse_fallback = || -> Option<PlayTarget> {
        let browse = find_first(item.get("navigationEndpoint").or_else(|| item.get("onTap"))?, "browseEndpoint")?;
        let id = browse.get("browseId")?.as_str()?;
        if let Some(playlist) = id.strip_prefix("VL") {
            return Some(PlayTarget {
                source: super::ID,
                track: None,
                playlist: Some(playlist.to_string()),
                browse: None,
            });
        }
        let page_type = find_first(browse, "pageType").and_then(Value::as_str);
        let playable_browse = matches!(page_type, Some("MUSIC_PAGE_TYPE_ALBUM" | "MUSIC_PAGE_TYPE_ARTIST"))
            || id.starts_with("MPREb_")
            || id.starts_with("UC");
        playable_browse.then(|| PlayTarget {
            source: super::ID,
            track: None,
            playlist: None,
            browse: Some(id.to_string()),
        })
    };
    endpoint(item.get("thumbnailOverlay").or_else(|| item.get("overlay")))
        .or_else(|| endpoint(item.get("buttons")))
        .or_else(|| endpoint(item.get("navigationEndpoint").or_else(|| item.get("onTap"))))
        .or_else(|| endpoint(radio))
        .or_else(browse_fallback)
}

/// Song or video, from the watch endpoint's music video type.
fn watch_kind(item: &Value) -> Option<MusicKind> {
    let video_type = find_first(item, "musicVideoType")?.as_str()?;
    Some(if video_type == "MUSIC_VIDEO_TYPE_ATV" { MusicKind::Song } else { MusicKind::Video })
}

fn page_kind(item: &Value) -> Option<MusicKind> {
    let nav = item.get("navigationEndpoint").or_else(|| item.get("onTap"))?;
    let page_type = find_first(nav, "pageType")?.as_str()?;
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

/// A song or search result row: the title, then columns of artists, album, plays.
fn list_item(item: &Value) -> Option<MusicItem> {
    let columns: Vec<String> = item
        .get("flexColumns")?
        .as_array()?
        .iter()
        .filter_map(|column| column.pointer("/musicResponsiveListItemFlexColumnRenderer/text").and_then(text))
        .filter(|text| !text.is_empty())
        .collect();
    let (title, rest) = columns.split_first()?;
    let kind = page_kind(item).or_else(|| watch_kind(item)).unwrap_or(MusicKind::Song);
    Some(MusicItem {
        kind,
        title: title.clone(),
        subtitle: rest.join(" • "),
        thumbnail_url: pick_thumbnail(
            &thumbnails(item.pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")),
            96,
        ),
        play: play_target(item),
    })
}

fn parse_search(response: &Value, filter: MusicSearchFilter) -> MusicSearchPage {
    let sections = response
        .pointer("/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut corrected_query = None;
    let mut shelves = Vec::new();
    let mut loose_items = Vec::new();
    let mut more = None;

    for section in sections {
        if let Some(item_section) = section.get("itemSectionRenderer") {
            let entries = item_section.get("contents").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
            for entry in entries {
                if let Some(corrected) = entry.pointer("/showingResultsForRenderer/correctedQuery").and_then(text) {
                    corrected_query = Some(corrected);
                } else if let Some(shelf) = entry.get("musicShelfRenderer") {
                    let (parsed, continuation) = search_shelf(shelf, filter);
                    if let Some(parsed) = parsed {
                        shelves.push(parsed);
                    }
                    if more.is_none() {
                        more = continuation;
                    }
                } else if let Some(item) = entry.get("musicResponsiveListItemRenderer").and_then(list_item) {
                    loose_items.push(item);
                } else if let Some(token) = entry
                    .pointer("/continuationItemRenderer/continuationEndpoint/continuationCommand/token")
                    .and_then(Value::as_str)
                    && more.is_none()
                {
                    more = Some(token.to_string());
                }
            }
        } else if let Some(card) = section.get("musicCardShelfRenderer") {
            if let Some(shelf) = search_card_shelf(card) {
                shelves.push(shelf);
            }
        } else if let Some(shelf) = section.get("musicShelfRenderer") {
            let (parsed, continuation) = search_shelf(shelf, filter);
            if let Some(parsed) = parsed {
                shelves.push(parsed);
            }
            if more.is_none() {
                more = continuation;
            }
        }
    }
    if !loose_items.is_empty() {
        shelves.push(MusicSearchShelf { title: "Results".to_string(), items: loose_items, filter: None });
    }

    MusicSearchPage { shelves, corrected_query, more: more.map(|token| MusicSearchMore { source: super::ID, token }) }
}

fn parse_search_continuation(response: &Value) -> MusicSearchPage {
    let mut shelves = Vec::new();
    let mut more = None;
    if let Some(shelf) = response.pointer("/continuationContents/musicShelfContinuation") {
        let (parsed, continuation) = search_shelf(shelf, MusicSearchFilter::All);
        if let Some(parsed) = parsed {
            shelves.push(parsed);
        }
        more = continuation;
    } else {
        let mut found = Vec::new();
        find_all(response, "musicShelfRenderer", &mut found);
        for shelf in found {
            let (parsed, continuation) = search_shelf(shelf, MusicSearchFilter::All);
            if let Some(parsed) = parsed {
                shelves.push(parsed);
            }
            if more.is_none() {
                more = continuation;
            }
        }
    }
    MusicSearchPage {
        shelves,
        corrected_query: None,
        more: more.map(|token| MusicSearchMore { source: super::ID, token }),
    }
}

fn search_card_shelf(card: &Value) -> Option<MusicSearchShelf> {
    let title = card
        .pointer("/header/musicCardShelfHeaderBasicRenderer/title")
        .and_then(text)
        .unwrap_or_else(|| "Top result".to_string());
    let mut items = Vec::new();
    if let Some(main_title) = card.get("title").and_then(text) {
        let kind = page_kind(card).or_else(|| watch_kind(card)).unwrap_or(MusicKind::Song);
        items.push(MusicItem {
            kind,
            title: main_title,
            subtitle: card.get("subtitle").and_then(text).unwrap_or_default(),
            thumbnail_url: pick_thumbnail(
                &thumbnails(card.pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")),
                226,
            ),
            play: play_target(card),
        });
    }
    if let Some(contents) = card.get("contents").and_then(Value::as_array) {
        items.extend(
            contents.iter().filter_map(|entry| entry.get("musicResponsiveListItemRenderer").and_then(list_item)),
        );
    }
    (!items.is_empty()).then_some(MusicSearchShelf { title, items, filter: None })
}

fn search_shelf(shelf: &Value, active_filter: MusicSearchFilter) -> (Option<MusicSearchShelf>, Option<String>) {
    let title = shelf.get("title").and_then(text).unwrap_or_else(|| {
        if active_filter == MusicSearchFilter::All { "Results" } else { active_filter.label() }.to_string()
    });
    let items: Vec<MusicItem> = shelf
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("musicResponsiveListItemRenderer").and_then(list_item))
        .collect();
    let continuation = shelf
        .pointer("/continuations/0/nextContinuationData/continuation")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            shelf.get("contents").and_then(Value::as_array).into_iter().flatten().find_map(|entry| {
                entry
                    .pointer("/continuationItemRenderer/continuationEndpoint/continuationCommand/token")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        });
    let filter =
        (active_filter == MusicSearchFilter::All).then(|| shelf_filter(&title, shelf.get("bottomEndpoint"))).flatten();
    let parsed = (!items.is_empty()).then_some(MusicSearchShelf { title, items, filter });
    (parsed, continuation)
}

fn shelf_filter(title: &str, bottom_endpoint: Option<&Value>) -> Option<MusicSearchFilter> {
    if let Some(params) = bottom_endpoint.and_then(|ep| ep.pointer("/searchEndpoint/params")).and_then(Value::as_str) {
        for filter in [
            MusicSearchFilter::Songs,
            MusicSearchFilter::Videos,
            MusicSearchFilter::Albums,
            MusicSearchFilter::Playlists,
            MusicSearchFilter::Artists,
        ] {
            if filter_params(filter) == Some(params) {
                return Some(filter);
            }
        }
    }
    match title.to_ascii_lowercase().as_str() {
        "songs" => Some(MusicSearchFilter::Songs),
        "videos" => Some(MusicSearchFilter::Videos),
        "albums" => Some(MusicSearchFilter::Albums),
        "artists" => Some(MusicSearchFilter::Artists),
        t if t.contains("playlist") => Some(MusicSearchFilter::Playlists),
        _ => None,
    }
}

fn parse_search_suggestions(response: &Value) -> MusicSearchSuggestions {
    let mut terms = Vec::new();
    let mut items = Vec::new();
    let sections = response.get("contents").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    for section in sections {
        let Some(contents) = section.pointer("/searchSuggestionsSectionRenderer/contents").and_then(Value::as_array)
        else {
            continue;
        };
        for entry in contents {
            if let Some(term) = entry.pointer("/searchSuggestionRenderer/suggestion").and_then(text) {
                terms.push(term);
            } else if let Some(item) = entry.get("musicResponsiveListItemRenderer").and_then(list_item) {
                items.push(item);
            }
        }
    }
    MusicSearchSuggestions { terms, items }
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
    fn reads_tv_music_shelves() {
        let tile = |command: Value, kind: &str, title: &str| {
            serde_json::json!({"tileRenderer": {"contentType": kind, "onSelectCommand": command,
                "header": {"tileHeaderRenderer": {"thumbnail": {"thumbnails": [{"url": "https://t/320", "width": 320}]}}},
                "metadata": {"tileMetadataRenderer": {"title": {"simpleText": title}, "lines": [
                    {"lineRenderer": {"items": [{"lineItemRenderer": {"text": {"simpleText": "Mina Okabe"}}}]}},
                    {"lineRenderer": {"items": [{"lineItemRenderer": {"text": {"simpleText": "Single"}}}]}}]}}}})
        };
        let response = serde_json::json!({"contents": {"sectionListRenderer": {"contents": [{"shelfRenderer": {
            "headerRenderer": {"shelfHeaderRenderer": {"avatarLockup": {"avatarLockupRenderer": {
                "title": {"runs": [{"text": "Listen again"}]}}}}},
            "content": {"horizontalListRenderer": {"items": [
                tile(serde_json::json!({"watchEndpoint": {"videoId": "r0joO1NE7I4", "playlistId": "RDr0joO1NE7I4"}}),
                     "TILE_CONTENT_TYPE_VIDEO", "偽顔"),
                tile(serde_json::json!({"browseEndpoint": {"browseId": "VLOLAK5uy_abc", "browseEndpointContextSupportedConfigs":
                     {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ALBUM"}}}}),
                     "TILE_CONTENT_TYPE_PLAYLIST", "Dancing Around The Truth")]}}}}]}}});
        let shelves: Vec<MusicShelf> = tv_shelves(&response).into_iter().map(|(shelf, _)| shelf).collect();
        assert_eq!(shelves.len(), 1);
        assert_eq!(shelves[0].title, "Listen again");
        let (song, album) = (&shelves[0].items[0], &shelves[0].items[1]);
        assert_eq!(
            song.play.as_ref().map(|play| (play.track.as_deref(), play.playlist.as_deref())),
            Some((Some("r0joO1NE7I4"), None))
        );
        assert_eq!((album.kind, album.subtitle.as_str()), (MusicKind::Album, "Mina Okabe • Single"));
        assert_eq!(album.play.as_ref().and_then(|play| play.playlist.as_deref()), Some("OLAK5uy_abc"));
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

    #[test]
    fn reads_music_search_and_suggestions() {
        let response = serde_json::json!({"contents": {"tabbedSearchResultsRenderer": {"tabs": [{"tabRenderer": {"content":
            {"sectionListRenderer": {"contents": [
                {"musicCardShelfRenderer": {
                    "header": {"musicCardShelfHeaderBasicRenderer": {"title": {"runs": [{"text": "Top result"}]}}},
                    "title": {"runs": [{"text": "Daft Punk"}]},
                    "subtitle": {"runs": [{"text": "Artist • 65M monthly audience"}]},
                    "onTap": {"browseEndpoint": {"browseId": "UC_artist", "browseEndpointContextSupportedConfigs":
                        {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ARTIST"}}}},
                    "buttons": [{"buttonRenderer": {"command": {"watchPlaylistEndpoint": {"playlistId": "RDAO_artist"}}}}],
                    "contents": [{"musicResponsiveListItemRenderer": {
                        "flexColumns": [
                            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Instant Crush"}]}}},
                            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Song • 5:38"}]}}}],
                        "overlay": {"musicItemThumbnailOverlayRenderer": {"content": {"musicPlayButtonRenderer": {
                            "playNavigationEndpoint": watch("khnokW3Mw24", "MUSIC_VIDEO_TYPE_ATV")}}}}}}]
                }},
                {"itemSectionRenderer": {"contents": [{"musicResponsiveListItemRenderer": {
                    "flexColumns": [
                        {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Discovery"}]}}},
                        {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Album • Daft Punk • 2001"}]}}}],
                    "navigationEndpoint": {"browseEndpoint": {"browseId": "MPREb_disc", "browseEndpointContextSupportedConfigs":
                        {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ALBUM"}}}}
                }}]}}
            ]}}}}]}}}
        );
        let page = parse_search(&response, MusicSearchFilter::All);
        assert_eq!(page.shelves.len(), 2);
        assert_eq!(page.shelves[0].title, "Top result");
        assert_eq!(page.shelves[0].items.len(), 2);
        assert_eq!(page.shelves[0].items[0].kind, MusicKind::Artist);
        assert_eq!(page.shelves[0].items[0].play.as_ref().and_then(|p| p.playlist.as_deref()), Some("RDAO_artist"));
        assert_eq!(page.shelves[0].items[1].kind, MusicKind::Song);
        assert_eq!(page.shelves[1].title, "Results");
        assert_eq!(page.shelves[1].items[0].kind, MusicKind::Album);
        assert_eq!(page.shelves[1].items[0].play.as_ref().and_then(|p| p.browse.as_deref()), Some("MPREb_disc"));

        let suggest = serde_json::json!({"contents": [{"searchSuggestionsSectionRenderer": {"contents": [
            {"searchSuggestionRenderer": {"suggestion": {"runs": [{"text": "daft punk"}]}}},
            {"musicResponsiveListItemRenderer": {
                "flexColumns": [
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Get Lucky"}]}}},
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Song • Daft Punk"}]}}}],
                "overlay": {"musicItemThumbnailOverlayRenderer": {"content": {"musicPlayButtonRenderer": {
                    "playNavigationEndpoint": watch("4D7u5KF7SP8", "MUSIC_VIDEO_TYPE_ATV")}}}}}}
        ]}}]});
        let parsed = parse_search_suggestions(&suggest);
        assert_eq!(parsed.terms, ["daft punk"]);
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].play.as_ref().and_then(|p| p.track.as_deref()), Some("4D7u5KF7SP8"));
    }
}
