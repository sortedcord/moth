//! Secrets (account credentials) in the desktop's keyring, through the Secret Service
//! (gnome-keyring, KWallet), never in plain files. Items are tagged `application=moth` plus a
//! key of the caller's choosing.

use anyhow::{Context as _, Result};

const APPLICATION: &str = "moth";

fn attributes(key: &str) -> [(&'static str, &str); 2] {
    [("application", APPLICATION), ("moth-key", key)]
}

async fn keyring() -> Result<oo7::Keyring> {
    oo7::Keyring::new().await.context("no keyring (Secret Service) available")
}

async fn unlocked() -> Result<oo7::Keyring> {
    let keyring = keyring().await?;
    keyring.unlock().await.context("the keyring is locked")?;
    Ok(keyring)
}

/// The secret stored under `key`, if any. Only unlocks the keyring (which may prompt) when
/// there's something to read.
pub async fn read(key: &str) -> Result<Option<String>> {
    let keyring = keyring().await?;
    let items = keyring.search_items(&attributes(key)).await.context("couldn't search the keyring")?;
    let Some(item) = items.first() else { return Ok(None) };
    item.unlock().await.context("the keyring is locked")?;
    let secret = item.secret().await.context("couldn't read from the keyring")?;
    Ok(Some(String::from_utf8(secret.to_vec()).context("a keyring item isn't text")?))
}

/// Store `secret` under `key`, replacing what was there. `label` is what keyring managers show.
pub async fn write(key: &str, label: &str, secret: &str) -> Result<()> {
    let keyring = unlocked().await?;
    keyring.create_item(label, &attributes(key), secret, true).await.context("couldn't write to the keyring")
}

pub async fn delete(key: &str) -> Result<()> {
    let keyring = keyring().await?;
    keyring.delete(&attributes(key)).await.context("couldn't delete from the keyring")
}
