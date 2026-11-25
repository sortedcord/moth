//! YouTube channel pages: a channel's details and videos (RustyPipe), whether the
//! recommendation account follows it, and following and unfollowing.
//!
//! Subscriptions go through InnerTube's `subscription/subscribe` and `subscription/unsubscribe`
//! with the recommendation account's sign-in, whichever kind it is. The channel's page, asked
//! for with that sign-in, says whether the account is subscribed (the TV app and the web word
//! it differently: see [`subscribed_state`]).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use rustypipe::client::{ClientType, RustyPipe};
use serde_json::Value;

use super::ID;
use super::account::Account;
use super::feed;
use crate::backend::media::{ChannelDetails, ChannelMore, ChannelOrder, ChannelRef, ChannelVideos, VideoSummary};
use crate::sync::lock;

/// Whether `input` is a link to a channel (`youtube.com/@name`, `/channel/UC…`, `/c/…`,
/// `/user/…`), a handle (`@name`) or a channel id.
pub fn claims(input: &str) -> bool {
    if is_channel_id(input) {
        return true;
    }
    if let Some(handle) = input.strip_prefix('@') {
        return is_handle(handle);
    }
    let rest = input.strip_prefix("https://").or_else(|| input.strip_prefix("http://")).unwrap_or(input);
    let Some((host, path)) = rest.split_once('/') else { return false };
    if !super::HOSTS.contains(&host) || host == "youtu.be" {
        return false;
    }
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let mut parts = path.split('/');
    match (parts.next(), parts.next()) {
        (Some(first), _) if first.starts_with('@') => is_handle(&first[1..]),
        (Some("channel" | "c" | "user"), Some(name)) => !name.is_empty(),
        _ => false,
    }
}

/// What identifies the channel in a link: `https://www.youtube.com/@name/videos?view=0` becomes
/// `https://www.youtube.com/@name` (RustyPipe can't resolve links with a tab or a query).
pub fn normalize(input: &str) -> String {
    let Some((scheme, rest)) = input.split_once("://") else {
        return match input.split_once('/') {
            Some((host, _)) if super::HOSTS.contains(&host) => normalize(&format!("https://{input}")),
            _ => input.to_string(),
        };
    };
    let Some((host, path)) = rest.split_once('/') else { return input.to_string() };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let mut parts = path.split('/');
    let kept = match (parts.next(), parts.next()) {
        (Some(handle), _) if handle.starts_with('@') => handle.to_string(),
        (Some(kind @ ("channel" | "c" | "user")), Some(name)) => format!("{kind}/{name}"),
        _ => return input.to_string(),
    };
    format!("{scheme}://{host}/{kept}")
}

/// `UC` and 22 URL-safe base64 characters.
fn is_channel_id(input: &str) -> bool {
    input.len() == 24
        && input.starts_with("UC")
        && input.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// What YouTube lets a handle be made of (3 to 30 characters).
fn is_handle(handle: &str) -> bool {
    (3..=30).contains(&handle.len())
        && handle.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '·'))
}

/// The Videos tab's `params` in a browse request.
const VIDEOS_TAB: &str = "EgZ2aWRlb3PyBgQKAjoA";

/// The channel's Videos tab, as the web shows it (latest first).
///
/// RustyPipe parses channels too, but 0.11.4 reads the subscriber count and video count out
/// of the new header wrongly, finds no view counts, and builds sort continuations YouTube
/// rejects. The page is read here instead, with the home feed's video parser.
async fn videos_tab(client: &RustyPipe, id: &str) -> Result<Value> {
    let body = serde_json::json!({"browseId": id, "params": VIDEOS_TAB});
    let response = client.query().raw(ClientType::Desktop, "browse", &body).await?;
    Ok(serde_json::from_str(&response)?)
}

/// A channel's page. `follower` is the account whose subscription to show, if it's signed in.
pub async fn details(client: &RustyPipe, follower: &Account, id: String) -> Result<ChannelDetails> {
    let (tab, subscribed) = tokio::join!(videos_tab(client, &id), subscribed(follower, &id));
    let tab = tab?;
    let mut details = header(&tab).ok_or_else(|| anyhow!("that channel doesn't exist, or YouTube changed its page"))?;
    details.subscribed = subscribed;
    details.videos = videos_page(&tab);
    Ok(details)
}

/// The channel's name, picture, counts and description from the page header.
fn header(tab: &Value) -> Option<ChannelDetails> {
    let id = tab.pointer("/metadata/channelMetadataRenderer/externalId")?.as_str()?;
    let header = tab.pointer("/header/pageHeaderRenderer/content/pageHeaderViewModel")?;
    let name = header.pointer("/title/dynamicTextViewModel/text/content").and_then(Value::as_str)?;
    let image = |pointer: &str, min_width| {
        let sources = feed::thumbnails(header.pointer(pointer));
        feed::pick_thumbnail(&sources, min_width)
    };
    let avatar = image("/image/decoratedAvatarViewModel/avatar/avatarViewModel/image/sources", 160);
    let banner = image("/banner/imageBannerViewModel/image/sources", 1280);
    let parts: Vec<String> = header
        .pointer("/metadata/contentMetadataViewModel/metadataRows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|row| row.get("metadataParts").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|part| part.get("text").and_then(feed::text))
        .collect();
    let counted = |word: &str| parts.iter().find(|part| part.contains(word)).and_then(|part| feed::count(part));
    let description = tab
        .pointer("/metadata/channelMetadataRenderer/description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    Some(ChannelDetails {
        channel: ChannelRef { source: ID, id: id.to_string() },
        name: name.to_string(),
        handle: parts.iter().find(|part| part.starts_with('@')).cloned(),
        subscribers: counted("subscriber"),
        video_count: counted("video"),
        avatar_url: avatar,
        banner_url: banner,
        description,
        verified: header.to_string().contains("CHECK_CIRCLE_FILLED"),
        subscribed: None,
        videos: ChannelVideos { videos: Vec::new(), more: None },
    })
}

/// The videos in a Videos tab or a continuation of it, and the continuation after them.
fn videos_page(response: &Value) -> ChannelVideos {
    // A channel's own tab doesn't say whose the videos are, and the parser would take the
    // views for the channel's name.
    let videos = feed::videos(response)
        .into_iter()
        .map(|video| VideoSummary { channel: None, channel_avatar_url: None, ..video })
        .collect();
    ChannelVideos { videos, more: continuation(response).map(|token| ChannelMore { source: ID, token }) }
}

/// The token that fetches what follows the videos listed (the grid's last item). The sort
/// chips have continuation tokens of their own: those aren't it.
fn continuation(response: &Value) -> Option<String> {
    match response {
        Value::Object(map) => {
            if let Some(item) = map.get("continuationItemRenderer") {
                return item.pointer("/continuationEndpoint/continuationCommand/token")?.as_str().map(str::to_string);
            }
            map.values().find_map(continuation)
        }
        Value::Array(items) => items.iter().find_map(continuation),
        _ => None,
    }
}

/// A channel's videos in an order (without its details).
///
/// The Videos tab lists the orders as chips, each with a continuation token that fetches the
/// videos in that order.
pub async fn videos(client: &RustyPipe, id: String, sort: ChannelOrder) -> Result<ChannelVideos> {
    let tab = videos_tab(client, &id).await?;
    if sort == ChannelOrder::Latest {
        return Ok(videos_page(&tab));
    }
    let token = sort_token(&tab, sort.name())
        .ok_or_else(|| anyhow!("the channel's videos can't be sorted by {}", sort.name().to_lowercase()))?;
    videos_more(client, ChannelMore { source: ID, token }).await
}

pub async fn videos_more(client: &RustyPipe, more: ChannelMore) -> Result<ChannelVideos> {
    let body = serde_json::json!({"continuation": more.token});
    let response = client.query().raw(ClientType::Desktop, "browse", &body).await?;
    Ok(videos_page(&serde_json::from_str(&response)?))
}

/// The continuation token of the sort chip called `name` ("Latest", "Popular", "Oldest").
fn sort_token(response: &Value, name: &str) -> Option<String> {
    match response {
        Value::Object(map) => {
            if let Some(chip) = map.get("chipViewModel")
                && chip.get("text").and_then(Value::as_str) == Some(name)
                && let Some(token) = chip.pointer("/tapCommand/innertubeCommand/continuationCommand/token")
            {
                return token.as_str().map(str::to_string);
            }
            map.values().find_map(|value| sort_token(value, name))
        }
        Value::Array(items) => items.iter().find_map(|value| sort_token(value, name)),
        _ => None,
    }
}

/// Whether the account follows the channel; `None` if it isn't signed in or YouTube didn't
/// say (the reason is logged).
async fn subscribed(account: &Account, id: &str) -> Option<bool> {
    let (query, client) = account.query()?;
    let result = async {
        let response = query.raw(client, "browse", &serde_json::json!({"browseId": id})).await?;
        let response: Value = serde_json::from_str(&response)?;
        subscribed_state(&response, id).ok_or_else(|| anyhow!("the channel page has no subscribe button"))
    };
    match result.await {
        Ok(state) => Some(state),
        Err(err) => {
            eprintln!("moth: couldn't tell whether the recommendation account follows {id}: {err:#}");
            None
        }
    }
}

/// The subscribe button's state for channel `id` in a browse response: the TV app's and the
/// web's older `subscribeButtonRenderer` has `subscribed`; the web's newer
/// `subscribeButtonViewModel` has it under `subscribeState`, and names the channel only in
/// the command it runs.
fn subscribed_state(response: &Value, id: &str) -> Option<bool> {
    match response {
        Value::Object(map) => {
            if let Some(button) = map.get("subscribeButtonRenderer")
                && button.get("channelId").and_then(Value::as_str) == Some(id)
                && let Some(state) = button.get("subscribed").and_then(Value::as_bool)
            {
                return Some(state);
            }
            if let Some(button) = map.get("subscribeButtonViewModel") {
                let for_channel = button.to_string().contains(id);
                let state =
                    button.pointer("/subscribeButtonContent/subscribeState/subscribed").and_then(Value::as_bool);
                if let (true, Some(state)) = (for_channel, state) {
                    return Some(state);
                }
            }
            map.values().find_map(|value| subscribed_state(value, id))
        }
        Value::Array(items) => items.iter().find_map(|value| subscribed_state(value, id)),
        _ => None,
    }
}

/// How long YouTube's pages may report an old subscription state after a change.
const STATE_LAG: Duration = Duration::from_secs(60);

/// The subscriptions made or ended recently, which win over what a page says until it catches up.
#[derive(Default)]
pub struct RecentSubscriptions(Mutex<HashMap<String, (bool, Instant)>>);

impl RecentSubscriptions {
    pub fn set(&self, channel_id: String, subscribed: bool) {
        lock(&self.0).insert(channel_id, (subscribed, Instant::now()));
    }

    pub fn get(&self, channel_id: &str) -> Option<bool> {
        let mut recent = lock(&self.0);
        recent.retain(|_, (_, when)| when.elapsed() < STATE_LAG);
        recent.get(channel_id).map(|(subscribed, _)| *subscribed)
    }
}

/// Subscribe the account to a channel, or unsubscribe it.
pub async fn subscribe(account: &Account, id: String, subscribed: bool) -> Result<()> {
    let (query, client) = account.query().ok_or_else(|| anyhow!("sign in a recommendation account to subscribe"))?;
    // The `params` are the ones YouTube's own buttons send, for any channel.
    let (endpoint, params) =
        if subscribed { ("subscription/subscribe", "EgIIAhgA") } else { ("subscription/unsubscribe", "CgIIAhgA") };
    query.raw(client, endpoint, &serde_json::json!({"channelIds": [id], "params": params})).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_channel_links_and_handles() {
        assert!(claims("@veritasium"));
        assert!(claims("UCHnyfMqiRRG1u-2MsSQLbXA"));
        assert!(claims("https://www.youtube.com/@veritasium"));
        assert!(claims("https://www.youtube.com/@veritasium/videos?view=0"));
        assert!(claims("youtube.com/channel/UCHnyfMqiRRG1u-2MsSQLbXA"));
        assert!(claims("https://m.youtube.com/c/LinusTechTips"));
        assert!(claims("https://www.youtube.com/user/enyay"));
        assert!(!claims("https://www.youtube.com/watch?v=aqz-KE-bpKQ"));
        assert!(!claims("https://youtu.be/aqz-KE-bpKQ"));
        assert!(!claims("https://www.youtube.com/@"));
        assert!(!claims("@a"));
        assert!(!claims("https://example.com/@veritasium"));
        assert!(!claims("programming"));
    }

    #[test]
    fn reads_a_channel_page() {
        let tab = serde_json::json!({
            "metadata": {"channelMetadataRenderer": {"externalId": "UCaaaaaaaaaaaaaaaaaaaaaa", "description": "About.\n"}},
            "header": {"pageHeaderRenderer": {"content": {"pageHeaderViewModel": {
                "title": {"dynamicTextViewModel": {"text": {"content": "Name",
                    "attachmentRuns": [{"element": {"type": {"imageType": {"image": {"sources": [
                        {"clientResource": {"imageName": "CHECK_CIRCLE_FILLED"}}]}}}}}]}}},
                "image": {"decoratedAvatarViewModel": {"avatar": {"avatarViewModel": {"image": {"sources": [
                    {"url": "https://x/avatar72", "width": 72}, {"url": "https://x/avatar160", "width": 160}]}}}}},
                "banner": {"imageBannerViewModel": {"image": {"sources": [{"url": "https://x/banner", "width": 2120}]}}},
                "metadata": {"contentMetadataViewModel": {"metadataRows": [
                    {"metadataParts": [{"text": {"content": "@name"}}]},
                    {"metadataParts": [{"text": {"content": "21.5M subscribers"}}, {"text": {"content": "553 videos"}}]}]}}}}}},
            "contents": [
                {"chipViewModel": {"text": "Popular", "tapCommand": {"innertubeCommand": {"continuationCommand": {"token": "chip"}}}}},
                {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "next"}}}}]
        });
        let details = header(&tab).expect("a header");
        assert_eq!((details.name.as_str(), details.handle.as_deref()), ("Name", Some("@name")));
        assert_eq!((details.subscribers, details.video_count), (Some(21_500_000), Some(553)));
        assert_eq!(details.description, "About.");
        assert!(details.verified);
        assert_eq!(details.avatar_url.as_deref(), Some("https://x/avatar160"));
        assert_eq!(details.banner_url.as_deref(), Some("https://x/banner"));
        // The sort chips' tokens aren't the continuation.
        let page = videos_page(&tab);
        assert_eq!(page.more.map(|more| more.token).as_deref(), Some("next"));
        assert_eq!(sort_token(&tab, "Popular").as_deref(), Some("chip"));
    }

    #[test]
    fn recent_subscriptions_win_until_they_expire() {
        let recent = RecentSubscriptions::default();
        assert_eq!(recent.get("UC1"), None);
        recent.set("UC1".into(), true);
        assert_eq!(recent.get("UC1"), Some(true));
        recent.set("UC1".into(), false);
        assert_eq!(recent.get("UC1"), Some(false));
        lock(&recent.0).insert("UC2".into(), (true, Instant::now() - STATE_LAG - Duration::from_secs(1)));
        assert_eq!(recent.get("UC2"), None);
    }

    #[test]
    fn finds_a_sort_chip() {
        let tab = serde_json::json!({"header": {"chipBarViewModel": {"chips": [
            {"chipViewModel": {"text": "Latest", "tapCommand": {"innertubeCommand": {"continuationCommand": {"token": "a"}}}}},
            {"chipViewModel": {"text": "Popular", "tapCommand": {"innertubeCommand": {"continuationCommand": {"token": "b"}}}}}]}}});
        assert_eq!(sort_token(&tab, "Popular").as_deref(), Some("b"));
        assert_eq!(sort_token(&tab, "Oldest"), None);
    }

    #[test]
    fn links_are_cut_down_to_the_channel() {
        assert_eq!(
            normalize("https://www.youtube.com/@LinusTechTips/videos?view=0"),
            "https://www.youtube.com/@LinusTechTips"
        );
        assert_eq!(normalize("youtube.com/@a_b/featured"), "https://youtube.com/@a_b");
        assert_eq!(
            normalize("https://m.youtube.com/channel/UCHnyfMqiRRG1u-2MsSQLbXA/about"),
            "https://m.youtube.com/channel/UCHnyfMqiRRG1u-2MsSQLbXA"
        );
        assert_eq!(normalize("https://www.youtube.com/c/LinusTechTips/"), "https://www.youtube.com/c/LinusTechTips");
        assert_eq!(normalize("@veritasium"), "@veritasium");
    }

    #[test]
    fn reads_the_subscribe_state_of_the_channel() {
        let tv = serde_json::json!({"header": {"buttons": [{"subscribeButtonRenderer":
            {"channelId": "UCaaaaaaaaaaaaaaaaaaaaaa", "subscribed": true}}]}});
        assert_eq!(subscribed_state(&tv, "UCaaaaaaaaaaaaaaaaaaaaaa"), Some(true));
        // Some other channel's button (a shelf of related channels) isn't this channel's.
        assert_eq!(subscribed_state(&tv, "UCbbbbbbbbbbbbbbbbbbbbbb"), None);
        let web = serde_json::json!({"actions": [{"subscribeButtonViewModel": {"subscribeButtonContent": {
            "subscribeState": {"subscribed": false},
            "onTapCommand": {"innertubeCommand": {"subscribeEndpoint": {"channelIds": ["UCaaaaaaaaaaaaaaaaaaaaaa"]}}}}}}]});
        assert_eq!(subscribed_state(&web, "UCaaaaaaaaaaaaaaaaaaaaaa"), Some(false));
    }
}
