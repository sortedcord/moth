//! Video lists in InnerTube browse responses (home feeds), whichever client they come from:
//! `compactVideoRenderer` (visionOS), `videoRenderer` / `gridVideoRenderer` and
//! `lockupViewModel` (web), `tileRenderer` (TV).
//!
//! Videos are found wherever they sit, in order and without duplicates: the shelves and grids
//! around them change shape more often than the videos do. Shorts and other non-video items
//! (playlists, channels, ads) are skipped.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::backend::media::{VideoRef, VideoSummary};

/// Every video in a browse response.
pub fn videos(response: &Value) -> Vec<VideoSummary> {
    fn walk(value: &Value, found: &mut Vec<VideoSummary>) {
        match value {
            Value::Object(map) => {
                let video = map.iter().find_map(|(key, value)| match key.as_str() {
                    "compactVideoRenderer" | "videoRenderer" | "gridVideoRenderer" => Some(video_renderer(value)),
                    "lockupViewModel" => Some(lockup(value)),
                    "tileRenderer" => Some(tile(value)),
                    _ => None,
                });
                match video {
                    Some(Some(video)) => {
                        if !found.iter().any(|known| known.video == video.video) {
                            found.push(video);
                        }
                    }
                    // Something other than a video (a playlist, a Short): nothing inside either.
                    Some(None) => {}
                    None => map.values().for_each(|value| walk(value, found)),
                }
            }
            Value::Array(items) => items.iter().for_each(|value| walk(value, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(response, &mut found);
    found
}

/// The text of a `{"simpleText": …}`, `{"runs": [{"text": …}, …]}` or `{"content": …}` value.
pub fn text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("simpleText").or_else(|| value.get("content")).and_then(Value::as_str) {
        return Some(text.to_string());
    }
    let runs = value.get("runs")?.as_array()?;
    let text: String = runs.iter().filter_map(|run| run.get("text")?.as_str()).collect();
    (!text.is_empty()).then_some(text)
}

fn summary(id: &str, title: String) -> VideoSummary {
    VideoSummary {
        video: VideoRef { source: super::ID, id: id.to_string() },
        title,
        channel: None,
        views: None,
        age: None,
        duration: None,
        thumbnail_url: None,
        is_short: false,
    }
}

/// `compactVideoRenderer`, `videoRenderer`, `gridVideoRenderer`: the same fields.
fn video_renderer(renderer: &Value) -> Option<VideoSummary> {
    let id = renderer.get("videoId")?.as_str()?;
    let field = |name: &str| renderer.get(name).and_then(text);
    let thumbnails: Vec<Thumbnail> = renderer
        .pointer("/thumbnail/thumbnails")
        .and_then(|thumbnails| serde_json::from_value(thumbnails.clone()).ok())
        .unwrap_or_default();
    Some(VideoSummary {
        channel: field("shortBylineText").or_else(|| field("longBylineText")).or_else(|| field("ownerText")),
        views: field("viewCountText").and_then(|views| count(&views)),
        age: field("publishedTimeText"),
        duration: field("lengthText").and_then(|length| clock(&length)),
        thumbnail_url: pick_thumbnail(&thumbnails, 336),
        ..summary(id, field("title")?)
    })
}

/// The web's newer `lockupViewModel`: a title, then rows of parts (channel; views · age).
fn lockup(lockup: &Value) -> Option<VideoSummary> {
    if lockup.get("contentType").and_then(Value::as_str) != Some("LOCKUP_CONTENT_TYPE_VIDEO") {
        return None;
    }
    let id = lockup.get("contentId")?.as_str()?;
    let metadata = lockup.pointer("/metadata/lockupMetadataViewModel")?;
    let title = metadata.get("title").and_then(text)?;
    let parts: Vec<String> = metadata
        .pointer("/metadata/contentMetadataViewModel/metadataRows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|row| row.get("metadataParts").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|part| part.get("text").and_then(text))
        .collect();
    let thumbnails: Vec<Thumbnail> = lockup
        .pointer("/contentImage/thumbnailViewModel/image/sources")
        .and_then(|sources| serde_json::from_value(sources.clone()).ok())
        .unwrap_or_default();
    let duration = find_text(lockup, "thumbnailBadgeViewModel").and_then(|badge| clock(&badge));
    Some(VideoSummary {
        channel: parts.first().cloned(),
        views: parts.iter().find(|part| part.contains("view")).and_then(|views| count(views)),
        age: parts.iter().find(|part| part.ends_with("ago")).cloned(),
        duration,
        thumbnail_url: pick_thumbnail(&thumbnails, 336),
        ..summary(id, title)
    })
}

/// The TV app's `tileRenderer`: a header (thumbnail, duration) and lines of metadata.
fn tile(tile: &Value) -> Option<VideoSummary> {
    if tile.get("contentType").and_then(Value::as_str) != Some("TILE_CONTENT_TYPE_VIDEO") {
        return None;
    }
    let id = tile
        .pointer("/onSelectCommand/watchEndpoint/videoId")
        .or_else(|| tile.get("contentId"))
        .and_then(Value::as_str)?;
    let header = tile.pointer("/header/tileHeaderRenderer");
    let overlay = header
        .and_then(|header| header.get("thumbnailOverlays"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|overlay| overlay.get("thumbnailOverlayTimeStatusRenderer"));
    // Shorts and live streams carry a style instead of a plain duration.
    if overlay.and_then(|overlay| overlay.get("style")).and_then(Value::as_str).is_some_and(|style| style != "DEFAULT")
    {
        return None;
    }
    let metadata = tile.pointer("/metadata/tileMetadataRenderer")?;
    let title = metadata.get("title").and_then(text)?;
    let lines: Vec<Vec<String>> = metadata
        .get("lines")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|line| {
            line.pointer("/lineRenderer/items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| item.pointer("/lineItemRenderer/text").and_then(text))
                .filter(|part| part.trim() != "•")
                .collect()
        })
        .collect();
    let parts: Vec<&String> = lines.iter().flatten().collect();
    let thumbnails: Vec<Thumbnail> = header
        .and_then(|header| header.pointer("/thumbnail/thumbnails"))
        .and_then(|thumbnails| serde_json::from_value(thumbnails.clone()).ok())
        .unwrap_or_default();
    Some(VideoSummary {
        channel: lines.first().and_then(|line| line.first()).cloned(),
        views: parts.iter().find(|part| part.contains("view")).and_then(|views| count(views)),
        age: parts.iter().find(|part| part.ends_with("ago")).map(|age| age.to_string()),
        duration: overlay.and_then(|overlay| overlay.get("text")).and_then(text).and_then(|length| clock(&length)),
        thumbnail_url: pick_thumbnail(&thumbnails, 336),
        ..summary(id, title)
    })
}

/// The first text inside a `key` object anywhere under `value`.
fn find_text(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(map) => map
            .get(key)
            .and_then(|inner| inner.get("text"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| map.values().find_map(|value| find_text(value, key))),
        Value::Array(items) => items.iter().find_map(|value| find_text(value, key)),
        _ => None,
    }
}

#[derive(Deserialize)]
struct Thumbnail {
    url: String,
    #[serde(default)]
    width: u32,
}

/// Smallest thumbnail at least `min_width` wide, else the largest.
fn pick_thumbnail(thumbnails: &[Thumbnail], min_width: u32) -> Option<String> {
    thumbnails
        .iter()
        .filter(|thumbnail| thumbnail.width >= min_width)
        .min_by_key(|thumbnail| thumbnail.width)
        .or_else(|| thumbnails.iter().max_by_key(|thumbnail| thumbnail.width))
        .map(|thumbnail| thumbnail.url.clone())
}

/// "3,822,705 views" → 3822705; "1.2M views" → 1200000; "12K views" → 12000.
fn count(text: &str) -> Option<u64> {
    let number: String =
        text.trim().chars().take_while(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | 'K' | 'M' | 'B')).collect();
    let (digits, scale) = match number.chars().last()? {
        'K' => (&number[..number.len() - 1], 1e3),
        'M' => (&number[..number.len() - 1], 1e6),
        'B' => (&number[..number.len() - 1], 1e9),
        _ => (number.as_str(), 1.0),
    };
    if scale == 1.0 {
        return digits.replace([',', '.'], "").parse().ok();
    }
    let value: f64 = digits.replace(',', "").parse().ok()?;
    Some((value * scale).round() as u64)
}

/// "1:02:26" → 3746 s.
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

    #[test]
    fn reads_compact_video_shelves() {
        let json = r#"{"contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {"content":
            {"sectionListRenderer": {"contents": [
              {"shelfRenderer": {"title": {"runs": [{"text": "Recommended"}]}, "content": {"verticalListRenderer": {"items": [
                {"compactVideoRenderer": {"videoId": "iJC87bWWuPM", "title": {"runs": [{"text": "I Investigated the Village"}]},
                 "shortBylineText": {"runs": [{"text": "Nishu Tiwari"}]}, "viewCountText": {"simpleText": "3,822,705 views"},
                 "publishedTimeText": {"simpleText": "1 day ago"}, "lengthText": {"simpleText": "30:20"},
                 "thumbnail": {"thumbnails": [{"url": "https://i.ytimg.com/a/mq.jpg", "width": 320},
                                              {"url": "https://i.ytimg.com/a/hq720.jpg", "width": 1280}]}}}]}}}},
              {"shelfRenderer": {"content": {"verticalListRenderer": {"items": [
                {"compactVideoRenderer": {"videoId": "iJC87bWWuPM", "title": {"simpleText": "duplicate"}}},
                {"compactVideoRenderer": {"videoId": "8LRH9UpZc_s", "title": {"simpleText": "Jetha"},
                 "lengthText": {"simpleText": "1:02:26"}}}]}}}}
            ]}}}}]}}}"#;
        let videos = videos(&serde_json::from_str(json).unwrap());
        assert_eq!(videos.len(), 2);
        let first = &videos[0];
        assert_eq!((first.video.id.as_str(), first.title.as_str()), ("iJC87bWWuPM", "I Investigated the Village"));
        assert_eq!((first.channel.as_deref(), first.views), (Some("Nishu Tiwari"), Some(3_822_705)));
        assert_eq!(first.duration, Some(Duration::from_secs(30 * 60 + 20)));
        assert_eq!(first.thumbnail_url.as_deref(), Some("https://i.ytimg.com/a/hq720.jpg"));
        assert_eq!(videos[1].duration, Some(Duration::from_secs(3746)));
    }

    #[test]
    fn reads_tv_tiles_and_skips_shorts() {
        let tile = |id: &str, style: &str| {
            serde_json::json!({"tileRenderer": {"contentType": "TILE_CONTENT_TYPE_VIDEO", "contentId": id,
                "onSelectCommand": {"watchEndpoint": {"videoId": id}},
                "header": {"tileHeaderRenderer": {"thumbnail": {"thumbnails": [{"url": "https://t/hq.jpg", "width": 480}]},
                    "thumbnailOverlays": [{"thumbnailOverlayTimeStatusRenderer": {"text": {"simpleText": "12:34"}, "style": style}}]}},
                "metadata": {"tileMetadataRenderer": {"title": {"simpleText": format!("Video {id}")}, "lines": [
                    {"lineRenderer": {"items": [{"lineItemRenderer": {"text": {"runs": [{"text": "Veritasium"}]}}}]}},
                    {"lineRenderer": {"items": [{"lineItemRenderer": {"text": {"simpleText": "1.2M views"}}},
                        {"lineItemRenderer": {"text": {"simpleText": "•"}}},
                        {"lineItemRenderer": {"text": {"simpleText": "3 days ago"}}}]}}]}}}})
        };
        let response = serde_json::json!({"contents": {"tvBrowseRenderer": {"content": {"items": [
            tile("aaaaaaaaaaa", "DEFAULT"), tile("bbbbbbbbbbb", "SHORTS")]}}}});
        let videos = videos(&response);
        assert_eq!(videos.len(), 1);
        let video = &videos[0];
        assert_eq!((video.title.as_str(), video.channel.as_deref()), ("Video aaaaaaaaaaa", Some("Veritasium")));
        assert_eq!((video.views, video.age.as_deref()), (Some(1_200_000), Some("3 days ago")));
        assert_eq!(video.duration, Some(Duration::from_secs(754)));
    }

    #[test]
    fn reads_web_lockups() {
        let response = serde_json::json!({"richItemRenderer": {"content": {"lockupViewModel": {
            "contentType": "LOCKUP_CONTENT_TYPE_VIDEO", "contentId": "ccccccccccc",
            "contentImage": {"thumbnailViewModel": {"image": {"sources": [{"url": "https://t/c.jpg", "width": 480}]},
                "overlays": [{"thumbnailOverlayBadgeViewModel": {"thumbnailBadges": [
                    {"thumbnailBadgeViewModel": {"text": "8:05"}}]}}]}},
            "metadata": {"lockupMetadataViewModel": {"title": {"content": "A lockup"}, "metadata": {
                "contentMetadataViewModel": {"metadataRows": [
                    {"metadataParts": [{"text": {"content": "Some Channel"}}]},
                    {"metadataParts": [{"text": {"content": "45K views"}}, {"text": {"content": "2 weeks ago"}}]}]}}}}}}}});
        let videos = videos(&response);
        assert_eq!(videos.len(), 1);
        let video = &videos[0];
        assert_eq!(
            (video.title.as_str(), video.channel.as_deref(), video.views),
            ("A lockup", Some("Some Channel"), Some(45_000))
        );
        assert_eq!((video.age.as_deref(), video.duration), (Some("2 weeks ago"), Some(Duration::from_secs(485))));
        assert_eq!(video.thumbnail_url.as_deref(), Some("https://t/c.jpg"));
    }

    #[test]
    fn reads_counts() {
        assert_eq!(count("3,822,705 views"), Some(3_822_705));
        assert_eq!(count("1.2M views"), Some(1_200_000));
        assert_eq!(count("12K views"), Some(12_000));
        assert_eq!(count("No views"), None);
    }
}
