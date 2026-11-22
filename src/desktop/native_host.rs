//! The browser extension's native-messaging host. A Chromium-based browser starts moth with the
//! calling extension's origin as the first argument; moth reads one message from stdin, passes
//! it on to the running instance, and replies `{"ok": …}`:
//! - `{"url": …}`: open the link (launching moth if it isn't running).
//! - `{"account": "playback" | "recommendations", "cookies": …}`: sign that account in with the
//!   browser's YouTube session (moth must be running, to show how it went).

use std::io::{Read, Write};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

use super::socket;
use crate::backend::accounts::Role;

/// Whether moth was started by a browser as a native-messaging host.
pub fn is_invocation(args: &[String]) -> bool {
    args.first().is_some_and(|arg| arg.starts_with("chrome-extension://"))
}

/// Run one exchange; returns the process exit code.
pub fn run() -> i32 {
    let reply = match handle_message() {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(err) => serde_json::json!({ "ok": false, "error": format!("{err:#}") }),
    };
    match write_message(&reply) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn handle_message() -> Result<()> {
    let message = read_message()?;
    if let Some(role) = message.get("account").and_then(|role| role.as_str()) {
        let role = Role::parse(role).with_context(|| format!("no such account: {role}"))?;
        let cookies =
            message.get("cookies").and_then(|cookies| cookies.as_str()).context("message has no \"cookies\"")?;
        return socket::share_account(role, cookies)
            .context("moth isn't running; start it and share the account again");
    }
    let url = message.get("url").and_then(|url| url.as_str()).context("message has no \"url\"")?.trim().to_string();
    if !crate::backend::source::claims_url(&url) {
        bail!("not a link moth can open: {url}");
    }
    if socket::send_to_running(&url).is_ok() {
        return Ok(());
    }
    // No moth running: start one. Its own process group keeps it alive when the browser
    // terminates this host process after the reply.
    let exe = std::env::current_exe().context("can't locate the moth executable")?;
    Command::new(exe)
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("failed to start moth")?;
    Ok(())
}

/// Messages are a 32-bit native-endian length followed by that many bytes of UTF-8 JSON.
fn read_message() -> Result<serde_json::Value> {
    let mut stdin = std::io::stdin().lock();
    let mut length = [0u8; 4];
    stdin.read_exact(&mut length).context("no message from the browser")?;
    let length = u32::from_ne_bytes(length) as usize;
    if length > 64 * 1024 {
        bail!("message too large ({length} bytes)");
    }
    let mut body = vec![0u8; length];
    stdin.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

fn write_message(value: &serde_json::Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&(body.len() as u32).to_ne_bytes())?;
    stdout.write_all(&body)?;
    stdout.flush()?;
    Ok(())
}
