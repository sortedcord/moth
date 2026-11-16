//! MPRIS (`org.mpris.MediaPlayer2.moth` on the session bus): lets bars, media keys and
//! `playerctl` see what moth is playing and control it.
//!
//! The service reads the shared [`Hub`] snapshot; controls go to the UI thread as [`Command`]s.
//! zbus runs the service on its own executor thread.

use std::collections::HashMap;
use std::sync::Arc;

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::{Connection, interface};

use crate::ipc::{Command, Commands, Hub, NowPlaying};

const PATH: &str = "/org/mpris/MediaPlayer2";
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";
const MIN_RATE: f64 = 0.25;
const MAX_RATE: f64 = 2.0;
/// A position this far off from where playback should be counts as a seek.
const SEEK_THRESHOLD_SECONDS: f64 = 1.0;

pub struct Mpris {
    connection: Connection,
}

impl Mpris {
    pub fn start(hub: Arc<Hub>, commands: Commands) -> zbus::Result<Self> {
        let root = Root { commands: commands.clone() };
        let player = Player { hub, commands };
        let connection = zbus::block_on(async {
            let builder = zbus::connection::Builder::session()?.serve_at(PATH, root)?.serve_at(PATH, player)?;
            let connection = builder.build().await?;
            // Another moth (only one runs normally, but e.g. a second user session) keeps the
            // plain name; fall back to a per-process one like other players do.
            if connection.request_name("org.mpris.MediaPlayer2.moth").await.is_err() {
                connection.request_name(format!("org.mpris.MediaPlayer2.moth.instance{}", std::process::id())).await?;
            }
            zbus::Result::Ok(connection)
        })?;
        Ok(Self { connection })
    }

    /// Emit `PropertiesChanged` for whatever differs, and `Seeked` on position jumps.
    pub fn changed(&self, previous: &NowPlaying, now: &NowPlaying) {
        let result: zbus::Result<()> = zbus::block_on(async {
            let iface = self.connection.object_server().interface::<_, Player>(PATH).await?;
            let emitter = iface.signal_emitter();
            let player = iface.get().await;
            if playback_status(previous) != playback_status(now) {
                player.playback_status_changed(emitter).await?;
            }
            if metadata_key(previous) != metadata_key(now) {
                player.metadata_changed(emitter).await?;
            }
            if previous.rate != now.rate {
                player.rate_changed(emitter).await?;
            }
            if previous.video.is_some() != now.video.is_some() {
                player.can_play_changed(emitter).await?;
                player.can_pause_changed(emitter).await?;
                player.can_seek_changed(emitter).await?;
            }
            // `previous` is extrapolated to now, so any real difference is a jump (seek, or
            // a new video starting).
            if let (Some(before), Some(after)) = (previous.position, now.position)
                && (after - before).abs() > SEEK_THRESHOLD_SECONDS
            {
                Player::seeked(emitter, micros(after)).await?;
            }
            Ok(())
        });
        if let Err(err) = result {
            eprintln!("moth: MPRIS update failed: {err}");
        }
    }
}

fn micros(seconds: f64) -> i64 {
    (seconds * 1e6).round() as i64
}

fn playback_status(now: &NowPlaying) -> &'static str {
    match now.status {
        "playing" | "buffering" | "resolving" => "Playing",
        "paused" => "Paused",
        _ => "Stopped",
    }
}

/// Fields that feed `Metadata`, to tell whether it changed.
fn metadata_key(now: &NowPlaying) -> impl PartialEq {
    (
        now.video.clone(),
        now.duration.map(micros),
        now.stream
            .as_ref()
            .map(|stream| (stream.width, stream.height, stream.fps.map(|fps| fps.to_bits()), stream.quality.clone())),
    )
}

/// Object paths only allow `[A-Za-z0-9_]`; video IDs also use `-`.
fn track_id(video_id: &str) -> String {
    let mut path = String::from("/dev/moth/video/v");
    for byte in video_id.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => path.push(byte as char),
            other => path.push_str(&format!("_{other:02x}")),
        }
    }
    path
}

struct Root {
    commands: Commands,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {
        self.commands.try_send(Command::Raise).ok();
    }

    fn quit(&self) {
        self.commands.try_send(Command::Quit).ok();
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }

    /// Wayland only lets moth ask for attention, not take focus.
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> &str {
        "moth"
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec!["https".into()]
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Player {
    hub: Arc<Hub>,
    commands: Commands,
}

impl Player {
    fn now(&self) -> NowPlaying {
        self.hub.snapshot().now.clone()
    }

    fn send(&self, command: Command) {
        self.commands.try_send(command).ok();
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {}

    fn previous(&self) {}

    fn pause(&self) {
        self.send(Command::Pause);
    }

    fn play_pause(&self) {
        self.send(Command::PlayPause);
    }

    fn stop(&self) {
        self.send(Command::Stop);
    }

    fn play(&self) {
        self.send(Command::Play);
    }

    fn seek(&self, offset: i64) {
        self.send(Command::Seek(offset));
    }

    fn set_position(&self, track: ObjectPath<'_>, position: i64) {
        // Ignored unless it's for the current track (per the spec).
        let current = self.now().video.map(|video| track_id(&video.id));
        if current.as_deref() == Some(track.as_str()) {
            self.send(Command::SetPosition(position));
        }
    }

    fn open_uri(&self, uri: String) {
        self.send(Command::Open(uri));
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> &'static str {
        playback_status(&self.now())
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        self.now().rate
    }

    #[zbus(property)]
    fn set_rate(&mut self, rate: f64) {
        // A rate of 0 means pause, per the spec.
        if rate <= 0.0 {
            self.send(Command::Pause);
        } else {
            self.send(Command::SetRate(rate.clamp(MIN_RATE, MAX_RATE)));
        }
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let now = self.now();
        let mut metadata = HashMap::new();
        let mut insert = |key: &str, value: Value<'_>| {
            if let Ok(value) = OwnedValue::try_from(value) {
                metadata.insert(key.to_string(), value);
            }
        };
        let Some(video) = now.video else {
            insert("mpris:trackid", ObjectPath::from_static_str_unchecked(NO_TRACK).into());
            return metadata;
        };
        if let Ok(path) = ObjectPath::try_from(track_id(&video.id)) {
            insert("mpris:trackid", path.into());
        }
        insert("xesam:url", video.url.into());
        insert("mpris:artUrl", video.thumbnail.into());
        if let Some(title) = video.title {
            insert("xesam:title", title.into());
        }
        if let Some(channel) = video.channel {
            insert("xesam:artist", vec![channel].into());
        }
        if let Some(duration) = now.duration {
            insert("mpris:length", micros(duration).into());
        }
        // Non-standard, namespaced keys (allowed by the spec) for what MPRIS has no field for.
        if let Some(stream) = now.stream {
            if let (Some(width), Some(height)) = (stream.width, stream.height) {
                insert("moth:resolution", format!("{width}x{height}").into());
            }
            if let Some(fps) = stream.fps {
                insert("moth:fps", fps.into());
            }
            if let Some(quality) = stream.quality {
                insert("moth:quality", quality.into());
            }
            if let Some(codec) = stream.video_codec {
                insert("moth:videoCodec", codec.into());
            }
        }
        metadata
    }

    /// No volume control: always full; setting it does nothing.
    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn set_volume(&mut self, _volume: f64) {}

    /// Microseconds. Clients poll this; it isn't signalled (per the spec).
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.hub.snapshot().position().map_or(0, micros)
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        MIN_RATE
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        MAX_RATE
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.now().video.is_some()
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.now().video.is_some()
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.now().video.is_some()
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_ids_are_valid_object_paths() {
        for id in ["aqz-KE-bpKQ", "WhWc3b3KhnY", "a_b-c"] {
            let path = track_id(id);
            assert!(ObjectPath::try_from(path.as_str()).is_ok(), "{path}");
        }
        assert_ne!(track_id("a-b"), track_id("a_b"));
    }
}
