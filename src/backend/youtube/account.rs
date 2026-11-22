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

    /// An OAuth access token, for a device sign-in (refreshed as needed).
    pub async fn access_token(&self) -> Result<String> {
        Ok(self.client.user_auth_access_token().await?)
    }

    /// Look up the account's name. Errors leave the info as it was.
    pub async fn refresh_info(&self) -> Result<AccountInfo> {
        let method = self.method().ok_or_else(|| anyhow!("not signed in"))?;
        let (query, client) = self.query().ok_or_else(|| anyhow!("not signed in"))?;
        let response = query.raw(client, "account/accounts_list", &serde_json::json!({})).await?;
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
        self.sign_out_quietly().await;
        let result = if cookies.contains('\t') {
            self.client.user_auth_set_cookie_txt(cookies).await
        } else {
            self.client.user_auth_set_cookie(cookies).await
        };
        result.context("YouTube didn't accept those cookies as a signed-in session")?;
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

    /// Clear any previous sign-in before a new one, so an account has one method at a time.
    async fn sign_out_quietly(&self) {
        if self.method().is_some()
            && let Err(err) = self.sign_out().await
        {
            eprintln!("moth: {err:#}");
        }
    }
}

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
