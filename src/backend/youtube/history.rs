//! Watch history for the recommendation account: what YouTube's own apps report while a video
//! plays, so it shows up in the account's history (with how far it was watched) and informs
//! its recommendations.
//!
//! A signed-in player response carries the account's `playbackTracking` URLs. Fetching
//! `videostatsPlaybackUrl` (with a fresh playback nonce, `cpn`) records the view; fetching
//! `videostatsWatchtimeUrl` with the range played (`st`/`et`) and the current time (`cmt`)
//! records progress. The same requests yt-dlp's `--mark-watched` makes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use rand::Rng as _;

use super::account::Account;
use crate::backend::accounts::Method;
use crate::sync::lock;

/// The tracking URLs for one video being watched.
struct Tracking {
    cpn: String,
    watchtime_url: String,
    /// Where the current stretch of watching began.
    from: Duration,
}

#[derive(Default)]
pub struct History {
    watching: Mutex<HashMap<String, Tracking>>,
}

impl History {
    /// Record that `id` started playing at `position`.
    pub async fn started(&self, account: &Account, http: &reqwest::Client, id: &str, position: Duration) -> Result<()> {
        let (query, client) = account.query().ok_or_else(|| anyhow!("not signed in"))?;
        let body = serde_json::json!({"videoId": id, "contentCheckOk": true, "racyCheckOk": true});
        let response = query.raw(client, "player", &body).await?;
        let response: serde_json::Value = serde_json::from_str(&response).context("unexpected player response")?;
        let url = |key: &str| {
            response
                .pointer(&format!("/playbackTracking/{key}/baseUrl"))
                .and_then(|url| url.as_str())
                .map(str::to_string)
        };
        let (Some(playback_url), Some(watchtime_url)) = (url("videostatsPlaybackUrl"), url("videostatsWatchtimeUrl"))
        else {
            bail!("the player response has no tracking URLs");
        };
        let cpn = playback_nonce();
        let seconds = format!("{:.3}", position.as_secs_f64());
        let params = [("ver", "2"), ("cpn", &cpn), ("cmt", &seconds), ("el", "detailpage")];
        ping(account, http, &playback_url, &params).await?;
        lock(&self.watching).insert(id.to_string(), Tracking { cpn, watchtime_url, from: position });
        Ok(())
    }

    /// Record that `id` played from where it started (or was last reported) to `position`.
    pub async fn progress(
        &self,
        account: &Account,
        http: &reqwest::Client,
        id: &str,
        position: Duration,
    ) -> Result<()> {
        let (cpn, url, from) = {
            let mut watching = lock(&self.watching);
            let tracking = watching.get_mut(id).ok_or_else(|| anyhow!("{id} isn't being tracked"))?;
            let from = std::mem::replace(&mut tracking.from, position);
            (tracking.cpn.clone(), tracking.watchtime_url.clone(), from)
        };
        let (start, end) = (format!("{:.3}", from.as_secs_f64()), format!("{:.3}", position.as_secs_f64()));
        let params = [("ver", "2"), ("cpn", &cpn), ("cmt", &end), ("st", &start), ("et", &end), ("el", "detailpage")];
        ping(account, http, &url, &params).await
    }

    /// Record a seek: played up to `before`, carrying on from `after`.
    pub async fn jumped(
        &self,
        account: &Account,
        http: &reqwest::Client,
        id: &str,
        before: Duration,
        after: Duration,
    ) -> Result<()> {
        let result = self.progress(account, http, id, before).await;
        if let Some(tracking) = lock(&self.watching).get_mut(id) {
            tracking.from = after;
        }
        result
    }

    /// Stop tracking `id` (after its last progress report).
    pub fn stopped(&self, id: &str) {
        lock(&self.watching).remove(id);
    }
}

/// GET a tracking URL with extra parameters, signed in as `account`.
async fn ping(account: &Account, http: &reqwest::Client, url: &str, params: &[(&str, &str)]) -> Result<()> {
    let mut url = reqwest::Url::parse(url).context("bad tracking URL")?;
    {
        let keys: Vec<&str> = params.iter().map(|(key, _)| *key).collect();
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| !keys.contains(&key.as_ref()))
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let mut query = url.query_pairs_mut();
        query.clear().extend_pairs(kept).extend_pairs(params.iter().copied());
    }
    let request = http.get(url).timeout(Duration::from_secs(15));
    let request = match account.method() {
        Some(Method::DeviceCode) => request.bearer_auth(account.access_token().await?),
        Some(Method::Cookies) => request.header("Cookie", account.cookie_header().unwrap_or_default()),
        None => bail!("not signed in"),
    };
    let response = request.send().await.context("tracking request failed")?;
    if !response.status().is_success() {
        bail!("tracking request: HTTP {}", response.status());
    }
    Ok(())
}

/// A client playback nonce: 16 characters of URL-safe base64, random per playback.
fn playback_nonce() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut rng = rand::rng();
    (0..16).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonces_are_16_url_safe_characters() {
        let nonce = playback_nonce();
        assert_eq!(nonce.len(), 16);
        assert!(nonce.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));
        assert_ne!(nonce, playback_nonce());
    }
}
