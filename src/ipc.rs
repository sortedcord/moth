//! Single-instance handoff, status reporting, and the browser extension's native-messaging host.
//!
//! A running moth listens on `$XDG_RUNTIME_DIR/moth.sock`. Clients send one line:
//! - a URL or video ID: open it (answered with `ok`). `moth <url>` does this and exits, or becomes
//!   the instance if none is running.
//! - `status`: answered with one JSON line describing what's playing ([`NowPlaying`]).
//! - `watch`: the same JSON line now, then another every time it changes (the position counts in
//!   whole seconds), until the client hangs up. `moth status [--watch]` wraps both.
//!
//! When a Chromium-based browser starts moth as a native-messaging host (its first argument is
//! the calling extension's origin), moth reads one `{"url": …}` message from stdin, forwards
//! it to the running instance or launches a new one, and replies `{"ok": …}` on stdout.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command as Process, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use serde::Serialize;

/// Requests from outside (socket, MPRIS) for the UI thread.
#[derive(Debug)]
pub enum Command {
    Open(String),
    Play,
    Pause,
    PlayPause,
    /// Close the video.
    Stop,
    /// Relative seek, in microseconds.
    Seek(i64),
    /// Absolute position, in microseconds.
    SetPosition(i64),
    SetRate(f64),
    Raise,
    Quit,
}

pub type Commands = async_channel::Sender<Command>;

/// What moth is doing, as reported over the socket and MPRIS.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct NowPlaying {
    pub running: bool,
    /// "home", "watch" or "settings".
    pub page: &'static str,
    /// "idle", "resolving", "buffering", "playing", "paused", "ended" or "error".
    pub status: &'static str,
    pub error: Option<String>,
    pub video: Option<VideoInfo>,
    /// Seconds.
    pub position: Option<f64>,
    pub duration: Option<f64>,
    pub rate: f64,
    pub stream: Option<StreamInfo>,
    pub fullscreen: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct VideoInfo {
    pub id: String,
    pub url: String,
    /// Known once the watch page has loaded.
    pub title: Option<String>,
    pub channel: Option<String>,
    pub thumbnail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StreamInfo {
    /// Decoded frame size (or the stream's, before the first frame).
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// YouTube-style label, e.g. "1080p60".
    pub quality: Option<String>,
    /// Whether moth picks the quality (vs. one chosen in the menu).
    pub quality_auto: bool,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    /// GStreamer decoder element, e.g. "vavp9dec" (VA-API hardware).
    pub decoder: Option<String>,
    /// "rustypipe" or "yt-dlp".
    pub resolver: &'static str,
    /// Whether playback switched to web-client URLs (for seeking far ahead).
    pub fallback_urls: bool,
}

impl NowPlaying {
    pub fn not_running() -> Self {
        Self { page: "none", status: "idle", rate: 1.0, ..Default::default() }
    }

    /// Equal for reporting purposes: positions only count in whole seconds.
    fn same_as(&self, other: &Self) -> bool {
        let whole = |position: Option<f64>| position.map(|seconds| seconds.floor() as i64);
        whole(self.position) == whole(other.position)
            && Self { position: None, ..self.clone() } == Self { position: None, ..other.clone() }
    }
}

/// The latest [`NowPlaying`], with the time it was captured so the position can be extrapolated.
pub struct Snapshot {
    pub now: NowPlaying,
    pub at: Instant,
}

impl Snapshot {
    /// Position right now: the captured one, advanced by the time since if playing.
    pub fn position(&self) -> Option<f64> {
        let position = self.now.position?;
        Some(if self.now.status == "playing" {
            let advanced = position + self.at.elapsed().as_secs_f64() * self.now.rate;
            self.now.duration.map_or(advanced, |duration| advanced.min(duration))
        } else {
            position
        })
    }

    fn current(&self) -> NowPlaying {
        NowPlaying { position: self.position(), ..self.now.clone() }
    }
}

/// Shared between the UI (which publishes), the socket listener and the MPRIS service.
pub struct Hub {
    snapshot: Mutex<Snapshot>,
    subscribers: Mutex<Vec<UnixStream>>,
    changes: Mutex<Option<mpsc::Sender<(NowPlaying, NowPlaying)>>>,
}

impl Hub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshot: Mutex::new(Snapshot {
                now: NowPlaying { running: true, ..NowPlaying::not_running() },
                at: Instant::now(),
            }),
            subscribers: Mutex::new(Vec::new()),
            changes: Mutex::new(None),
        })
    }

    pub fn snapshot(&self) -> std::sync::MutexGuard<'_, Snapshot> {
        lock(&self.snapshot)
    }

    /// Record the current state. Cheap when nothing changed; changes are pushed to `watch`
    /// clients and MPRIS from a worker thread so the UI never waits on a slow client.
    pub fn publish(&self, now: NowPlaying) {
        let mut snapshot = lock(&self.snapshot);
        let previous = std::mem::replace(&mut *snapshot, Snapshot { now: now.clone(), at: Instant::now() });
        drop(snapshot);
        if !previous.now.same_as(&now)
            && let Some(changes) = lock(&self.changes).as_ref()
        {
            // Report the old state extrapolated to now, so a seek detector compares like with like.
            changes.send((previous.current(), now)).ok();
        }
    }

    /// Starts the thread that forwards changes; `on_change` runs there (for MPRIS signals).
    pub fn start_worker(self: &Arc<Self>, mut on_change: impl FnMut(&NowPlaying, &NowPlaying) + Send + 'static) {
        let (sender, receiver) = mpsc::channel::<(NowPlaying, NowPlaying)>();
        *lock(&self.changes) = Some(sender);
        let hub = self.clone();
        let spawned = std::thread::Builder::new().name("moth-status".into()).spawn(move || {
            while let Ok((previous, mut now)) = receiver.recv() {
                // Coalesce a backlog into one update.
                while let Ok((_, newer)) = receiver.try_recv() {
                    now = newer;
                }
                let line = status_line(&now);
                lock(&hub.subscribers).retain_mut(|stream| stream.write_all(line.as_bytes()).is_ok());
                on_change(&previous, &now);
            }
        });
        if let Err(err) = spawned {
            eprintln!("moth: couldn't start the status thread: {err}");
        }
    }
}

fn status_line(now: &NowPlaying) -> String {
    let mut line = serde_json::to_string(now).unwrap_or_else(|_| "{}".to_string());
    line.push('\n');
    line
}

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

/// Become the instance that others talk to. URLs to open arrive on `commands`; status requests
/// are answered from `hub`.
pub fn listen(commands: Commands, hub: Arc<Hub>) -> Result<()> {
    let path = socket_path();
    // A socket file nobody answers on is left over from a crash.
    if path.exists() && UnixStream::connect(&path).is_err() {
        std::fs::remove_file(&path).with_context(|| format!("failed to remove stale {}", path.display()))?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("failed to listen on {}", path.display()))?;
    std::thread::Builder::new().name("moth-ipc".into()).spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            // A client that connects and says nothing mustn't stall everyone else.
            stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
            stream.set_write_timeout(Some(Duration::from_millis(500))).ok();
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let reply = match line.trim() {
                "status" => status_line(&hub.snapshot().current()),
                "watch" => {
                    // Under the subscriber lock, so no change can slip in between this line and
                    // the subscription.
                    let mut subscribers = lock(&hub.subscribers);
                    if stream.write_all(status_line(&hub.snapshot().current()).as_bytes()).is_ok() {
                        subscribers.push(stream);
                    }
                    continue;
                }
                "" => "error\n".to_string(),
                url => {
                    let accepted = commands.send_blocking(Command::Open(url.to_string())).is_ok();
                    if accepted { "ok\n" } else { "error\n" }.to_string()
                }
            };
            if let Err(err) = stream.write_all(reply.as_bytes()) {
                eprintln!("moth: failed to answer IPC client: {err}");
            }
        }
    })?;
    Ok(())
}

/// `moth status [--watch]`: print the running instance's status as JSON lines. Without an
/// instance, prints `{"running":false,…}`; with `--watch`, keeps waiting for one to start.
pub fn run_status_cli(watch: bool) -> i32 {
    let not_running = status_line(&NowPlaying::not_running());
    let mut stdout = std::io::stdout().lock();
    let mut reported_down = false;
    loop {
        let streamed = (|| -> Result<()> {
            let mut stream = UnixStream::connect(socket_path())?;
            stream.set_read_timeout(if watch { None } else { Some(Duration::from_secs(5)) })?;
            writeln!(stream, "{}", if watch { "watch" } else { "status" })?;
            reported_down = false;
            for line in BufReader::new(stream).lines() {
                writeln!(stdout, "{}", line?)?;
                stdout.flush()?;
            }
            Ok(())
        })();
        if !watch {
            if streamed.is_err() {
                print!("{not_running}");
            }
            return 0;
        }
        // stdout closed (e.g. the bar went away): stop.
        if streamed.as_ref().is_err_and(|err| {
            err.downcast_ref::<std::io::Error>().is_some_and(|err| err.kind() == std::io::ErrorKind::BrokenPipe)
        }) {
            return 0;
        }
        if !reported_down {
            reported_down = true;
            if stdout.write_all(not_running.as_bytes()).and_then(|()| stdout.flush()).is_err() {
                return 0;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
    let url = message.get("url").and_then(|url| url.as_str()).context("message has no \"url\"")?.trim().to_string();
    if !is_youtube_url(&url) {
        bail!("not a YouTube URL: {url}");
    }
    if send_to_running(&url).is_ok() {
        return Ok(());
    }
    // No moth running: start one. Its own process group keeps it alive when the browser
    // terminates this host process after the reply.
    let exe = std::env::current_exe().context("can't locate the moth executable")?;
    Process::new(exe)
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
