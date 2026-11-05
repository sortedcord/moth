//! Single-instance handoff and the browser extension's native-messaging host.
//!
//! - A running moth listens on `$XDG_RUNTIME_DIR/moth.sock`; `moth <url>` hands the URL to it
//!   (one line, answered with `ok`) and exits, or becomes the instance if none is running.
//! - When a Chromium-based browser starts moth as a native-messaging host (its first argument is
//!   the calling extension's origin), moth reads one `{"url": …}` message from stdin, forwards
//!   it to the running instance or launches a new one, and replies `{"ok": …}` on stdout.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};

pub fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("moth.sock"),
        // SAFETY: getuid has no preconditions and cannot fail.
        None => std::env::temp_dir().join(format!("moth-{}.sock", unsafe { libc_getuid() })),
    }
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// Hand `url` to an already-running moth. Fails if there is none.
pub fn send_to_running(url: &str) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path())?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    writeln!(stream, "{}", url.trim())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    if reply.trim() != "ok" {
        bail!("moth replied {reply:?}");
    }
    Ok(())
}

/// Become the instance that others hand URLs to. URLs arrive on the returned channel.
pub fn listen() -> Result<mpsc::Receiver<String>> {
    let path = socket_path();
    // A socket file nobody answers on is left over from a crash.
    if path.exists() && UnixStream::connect(&path).is_err() {
        std::fs::remove_file(&path).with_context(|| format!("failed to remove stale {}", path.display()))?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("failed to listen on {}", path.display()))?;
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new().name("moth-ipc".into()).spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let url = line.trim().to_string();
            let accepted = !url.is_empty() && sender.send(url).is_ok();
            if let Err(err) = writeln!(stream, "{}", if accepted { "ok" } else { "error" }) {
                eprintln!("moth: failed to answer IPC client: {err}");
            }
        }
    })?;
    Ok(receiver)
}

pub fn remove_socket() {
    std::fs::remove_file(socket_path()).ok();
}

/// Whether moth was started by a browser as a native-messaging host.
pub fn is_native_host_invocation(args: &[String]) -> bool {
    args.first().is_some_and(|arg| arg.starts_with("chrome-extension://"))
}

/// Run one native-messaging exchange; returns the process exit code.
pub fn run_native_host() -> i32 {
    let reply = match handle_native_message() {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(err) => serde_json::json!({ "ok": false, "error": format!("{err:#}") }),
    };
    match write_native_message(&reply) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn handle_native_message() -> Result<()> {
    let message = read_native_message()?;
    let url = message
        .get("url")
        .and_then(|url| url.as_str())
        .context("message has no \"url\"")?
        .trim()
        .to_string();
    if !is_youtube_url(&url) {
        bail!("not a YouTube URL: {url}");
    }
    if send_to_running(&url).is_ok() {
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

fn is_youtube_url(url: &str) -> bool {
    ["https://www.youtube.com/", "https://youtube.com/", "https://m.youtube.com/", "https://youtu.be/"]
        .iter()
        .any(|prefix| url.starts_with(prefix))
}

/// Messages are a 32-bit native-endian length followed by that many bytes of UTF-8 JSON.
fn read_native_message() -> Result<serde_json::Value> {
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

fn write_native_message(value: &serde_json::Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&(body.len() as u32).to_ne_bytes())?;
    stdout.write_all(&body)?;
    stdout.flush()?;
    Ok(())
}
