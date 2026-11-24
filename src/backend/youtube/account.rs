//! Signed-in YouTube accounts: one RustyPipe client per [`Role`], signed in with a device code
//! (an OAuth token for YouTube's TV app) or a browser session's cookies.
//!
//! RustyPipe keeps the sign-in in its cache, and each account's cache lives in the keyring
//! ([`KeyringCache`]): written through as RustyPipe updates it (e.g. refreshing a token), and
//! deleted when it no longer holds a sign-in.

use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use rustypipe::cache::CacheStorage;
use rustypipe::client::{ClientType, OauthDeviceCode, RustyPipe, RustyPipeQuery};
use serde::Deserialize;
use tokio::sync::mpsc;

use super::feed::text;
use crate::backend::accounts::{AccountInfo, DeviceCode, Method, Role};
use crate::backend::secrets;
use crate::sync::lock;

pub struct Account {
    role: Role,
    client: RustyPipe,
    /// What RustyPipe last stored (it holds the session cookie, which RustyPipe doesn't expose).
    cache: Arc<Mutex<Option<String>>>,
    info: Mutex<Option<AccountInfo>>,
    /// Whether the session has YouTube Premium, and for which session (a hash of its cookies,
    /// so signing in as somebody else asks again).
    premium: Mutex<Option<(u64, bool)>>,
    /// A device sign-in waiting for the user.
    pending: tokio::sync::Mutex<Option<OauthDeviceCode>>,
    cancel: tokio::sync::Notify,
}

impl Account {
    /// The account for `role` as the keyring has it (signed out if it has nothing). Must run
    /// inside the tokio runtime.
    pub async fn load(role: Role) -> Result<Self> {
        let key = format!("youtube-{}", role.key());
        let stored = secrets::read(&key).await.unwrap_or_else(|err| {
            eprintln!("moth: couldn't read the YouTube {} from the keyring: {err:#}", role.name().to_lowercase());
            None
        });
        let cache = Arc::new(Mutex::new(stored.clone()));
        let (writes, pending_writes) = mpsc::unbounded_channel();
        let label = format!("moth: YouTube {}", role.name().to_lowercase());
        tokio::spawn(write_through(key, label, pending_writes));
        let storage = KeyringCache { stored, latest: cache.clone(), writes };
        let client = RustyPipe::builder()
            .storage(Box::new(storage))
            // Error reports would hold signed-in responses.
            .no_reporter()
            .build()?;
        let account = Self {
            role,
            client,
            cache,
            info: Mutex::new(None),
            premium: Mutex::new(None),
            pending: Default::default(),
            cancel: Default::default(),
        };
        if let Some(method) = account.method() {
            *lock(&account.info) = Some(AccountInfo { name: "Signed in".into(), handle: None, method });
        }
        Ok(account)
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// How the account is signed in, if it is.
    pub fn method(&self) -> Option<Method> {
        let query = self.client.query();
        if query.auth_enabled_client(&[ClientType::Tv]).is_some() {
            Some(Method::DeviceCode)
        } else if query.auth_enabled_client(&[ClientType::Desktop]).is_some() {
            Some(Method::Cookies)
        } else {
            None
        }
    }

    pub fn info(&self) -> Option<AccountInfo> {
        lock(&self.info).clone()
    }

    /// Signed-in requests, with the client type the sign-in works with.
    pub fn query(&self) -> Option<(RustyPipeQuery, ClientType)> {
        let client = match self.method()? {
            Method::DeviceCode => ClientType::Tv,
            Method::Cookies => ClientType::Desktop,
        };
        Some((self.client.query().authenticated(), client))
    }

    /// The session's cookie header, for a cookie sign-in.
    pub fn cookie_header(&self) -> Option<String> {
        #[derive(Deserialize)]
        struct Cache {
            auth_cookie: Option<AuthCookie>,
        }
        #[derive(Deserialize)]
        struct AuthCookie {
            cookie: String,
        }
        let cache = lock(&self.cache).clone()?;
        Some(serde_json::from_str::<Cache>(&cache).ok()?.auth_cookie?.cookie)
    }

    /// Whether a cookie sign-in has YouTube Premium: the web's top bar shows Premium's logo.
    /// Remembered per session; if YouTube can't be asked, it's assumed not (and asked again
    /// next time). A code sign-in never is: it can't get Premium's streams (#17).
    pub async fn is_premium(&self) -> bool {
        use std::hash::{Hash as _, Hasher as _};
        let Some(cookies) = self.cookie_header() else { return false };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        cookies.hash(&mut hasher);
        let session = hasher.finish();
        if let Some((known, premium)) = *lock(&self.premium)
            && known == session
        {
            return premium;
        }
        let result = async {
            let (query, client) = self.query().ok_or_else(|| anyhow!("not signed in"))?;
            let response = query.raw(client, "browse", &serde_json::json!({"browseId": "FEwhat_to_watch"})).await?;
            let response: serde_json::Value = serde_json::from_str(&response)?;
            let logo = response.pointer("/topbar/desktopTopbarRenderer/logo/topbarLogoRenderer/iconImage/iconType");
            Ok::<_, anyhow::Error>(logo.and_then(|logo| logo.as_str()) == Some("YOUTUBE_PREMIUM_LOGO"))
        };
        match result.await {
            Ok(premium) => {
                *lock(&self.premium) = Some((session, premium));
                premium
            }
            Err(err) => {
                eprintln!("moth: couldn't tell whether the playback account has Premium: {err:#}");
                false
            }
        }
    }

    /// An OAuth access token, for a device sign-in (refreshed as needed).
    pub async fn access_token(&self) -> Result<String> {
        Ok(self.client.user_auth_access_token().await?)
    }

    /// Look up the account's name. Errors leave the info as it was, except that a browser
    /// session YouTube no longer accepts is forgotten ([`SessionExpired`]).
    pub async fn refresh_info(&self) -> Result<AccountInfo> {
        let method = self.method().ok_or_else(|| anyhow!("not signed in"))?;
        if method == Method::Cookies
            && let Err(rustypipe::error::Error::Auth(_)) = self.client.user_auth_check_cookie().await
        {
            self.client.user_auth_remove_cookie().await.ok();
            *lock(&self.info) = None;
            return Err(SessionExpired.into());
        }
        let (query, client) = self.query().ok_or_else(|| anyhow!("not signed in"))?;
        // The TV app lists accounts; the web has the signed-in one in its account menu.
        let endpoint = match method {
            Method::DeviceCode => "account/accounts_list",
            Method::Cookies => "account/account_menu",
        };
        let response = query.raw(client, endpoint, &serde_json::json!({})).await?;
        let response: serde_json::Value = serde_json::from_str(&response).context("unexpected account list")?;
        let (name, handle) = selected_account(&response).ok_or_else(|| anyhow!("no account in the account list"))?;
        let info = AccountInfo { name, handle, method };
        *lock(&self.info) = Some(info.clone());
        Ok(info)
    }

    /// Start a device sign-in: show the user the code, then [`Self::finish_device_sign_in`].
    pub async fn begin_device_sign_in(&self) -> Result<DeviceCode> {
        let code = self.client.user_auth_get_code().await.context("YouTube didn't hand out a sign-in code")?;
        let shown = DeviceCode {
            code: code.user_code.clone(),
            url: code.verification_url.clone(),
            expires_in: std::time::Duration::from_secs(code.expires_in.into()),
        };
        *self.pending.lock().await = Some(code);
        Ok(shown)
    }

    /// Wait until the user has entered the code (or it expired, or was cancelled).
    pub async fn finish_device_sign_in(&self) -> Result<AccountInfo> {
        let code = self.pending.lock().await.take().ok_or_else(|| anyhow!("no sign-in in progress"))?;
        tokio::select! {
            result = self.client.user_auth_wait_for_login(&code) => {
                result.context("the sign-in didn't complete")?;
            }
            () = self.cancel.notified() => bail!("sign-in cancelled"),
        }
        // One method at a time: the code replaces a cookie sign-in.
        if self.cookie_header().is_some() {
            self.client.user_auth_remove_cookie().await.ok();
        }
        self.signed_in(Method::DeviceCode).await
    }

    /// Stop waiting for a device sign-in.
    pub fn cancel_device_sign_in(&self) {
        self.cancel.notify_waiters();
    }

    /// Sign in with a browser session: a `Cookie` header value, or a Netscape cookies.txt.
    pub async fn sign_in_with_cookies(&self, cookies: &str) -> Result<AccountInfo> {
        let cookies = cookies.trim();
        if cookies.is_empty() {
            bail!("no cookies");
        }
        let result = if cookies.contains('\t') {
            self.client.user_auth_set_cookie_txt(cookies).await
        } else {
            self.client.user_auth_set_cookie(cookies).await
        };
        // Cookies YouTube refuses aren't stored, and the previous sign-in stays.
        result.context("YouTube didn't accept those cookies as a signed-in session")?;
        // One method at a time: the session replaces a code sign-in (whose token is revoked).
        if self.client.query().auth_enabled_client(&[ClientType::Tv]).is_some()
            && let Err(err) = self.client.user_auth_logout().await
        {
            eprintln!("moth: couldn't revoke the previous sign-in: {err:#}");
        }
        self.signed_in(Method::Cookies).await
    }

    async fn signed_in(&self, method: Method) -> Result<AccountInfo> {
        match self.refresh_info().await {
            Ok(info) => Ok(info),
            Err(err) => {
                eprintln!("moth: signed in, but couldn't look up the account's name: {err:#}");
                let info = AccountInfo { name: "Signed in".into(), handle: None, method };
                *lock(&self.info) = Some(info.clone());
                Ok(info)
            }
        }
    }

    /// Sign out. A device sign-in's token is revoked; a cookie sign-in only forgets the cookies
    /// (signing the session out would sign the browser out too).
    pub async fn sign_out(&self) -> Result<()> {
        let result = match self.method() {
            Some(Method::DeviceCode) => self.client.user_auth_logout().await,
            Some(Method::Cookies) => self.client.user_auth_remove_cookie().await,
            None => Ok(()),
        };
        *lock(&self.info) = None;
        result.context("couldn't sign out")
    }
}

/// A browser session's cookies stopped working (YouTube rotated them, or the session was
/// signed out), so the account was signed out.
#[derive(Debug)]
pub struct SessionExpired;

impl std::fmt::Display for SessionExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The browser session expired, so this account was signed out. Share or paste a fresh one, or sign in with a code.")
    }
}

impl std::error::Error for SessionExpired {}

/// The selected account's name and handle in an `account/accounts_list` response.
fn selected_account(response: &serde_json::Value) -> Option<(String, Option<String>)> {
    fn walk<'a>(value: &'a serde_json::Value, found: &mut Vec<&'a serde_json::Value>) {
        match value {
            serde_json::Value::Object(map) => {
                if map.contains_key("accountName") {
                    found.push(value);
                }
                map.values().for_each(|value| walk(value, found));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|value| walk(value, found)),
            _ => {}
        }
    }
    let mut accounts = Vec::new();
    walk(response, &mut accounts);
    let selected = accounts
        .iter()
        .find(|account| account.get("isSelected").and_then(|selected| selected.as_bool()) == Some(true))
        .or(accounts.first())?;
    let name = selected.get("accountName").and_then(text)?;
    let handle = selected.get("channelHandle").and_then(text);
    Some((name, handle))
}

/// RustyPipe's cache for one account, kept in the keyring.
struct KeyringCache {
    stored: Option<String>,
    latest: Arc<Mutex<Option<String>>>,
    writes: mpsc::UnboundedSender<String>,
}

impl CacheStorage for KeyringCache {
    fn write(&self, data: &str) {
        *lock(&self.latest) = Some(data.to_string());
        self.writes.send(data.to_string()).ok();
    }

    fn read(&self) -> Option<String> {
        self.stored.clone()
    }
}

/// Store each cache RustyPipe writes; one without a sign-in deletes the item instead (there's
/// nothing worth keeping in the keyring then).
async fn write_through(key: String, label: String, mut writes: mpsc::UnboundedReceiver<String>) {
    while let Some(mut data) = writes.recv().await {
        // Only the latest matters.
        while let Ok(newer) = writes.try_recv() {
            data = newer;
        }
        let result =
            if holds_sign_in(&data) { secrets::write(&key, &label, &data).await } else { secrets::delete(&key).await };
        if let Err(err) = result {
            eprintln!("moth: {err:#}");
        }
    }
}

fn holds_sign_in(cache: &str) -> bool {
    let Ok(cache) = serde_json::from_str::<serde_json::Value>(cache) else { return false };
    ["oauth_token", "auth_cookie"].iter().any(|field| cache.get(field).is_some_and(|value| !value.is_null()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_selected_account() {
        let response = serde_json::json!({"contents": [{"accountSectionListRenderer": {"contents": [
        {"accountItemSectionRenderer": {"contents": [
            {"accountItem": {"accountName": {"simpleText": "Other"}, "isSelected": false}},
            {"accountItem": {"accountName": {"runs": [{"text": "Aditya"}]},
                             "channelHandle": {"simpleText": "@aditya"}, "isSelected": true}}
        ]}}]}}]});
        assert_eq!(selected_account(&response), Some(("Aditya".into(), Some("@aditya".into()))));
    }

    #[test]
    fn knows_a_cache_with_a_sign_in() {
        assert!(holds_sign_in(r#"{"clients": {}, "oauth_token": {"access_token": "a"}}"#));
        assert!(holds_sign_in(r#"{"auth_cookie": {"cookie": "SID=1"}}"#));
        assert!(!holds_sign_in(r#"{"clients": {}, "deobf": {}}"#));
        assert!(!holds_sign_in("not json"));
    }
}
