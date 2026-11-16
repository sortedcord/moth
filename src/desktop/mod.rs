//! Integration with the rest of the desktop: the instance socket (`moth <url>`, `moth status`),
//! MPRIS, and the browser extension's native-messaging host.
//!
//! Like the backend, nothing here depends on gpui: the UI publishes [`status::NowPlaying`] to the
//! [`status::Hub`] and receives [`Command`]s.

pub mod mpris;
pub mod native_host;
pub mod socket;
pub mod status;

use std::time::Duration;

/// Requests from outside (socket, MPRIS) for the UI thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Open(String),
    Play,
    Pause,
    PlayPause,
    /// Close the video.
    Stop,
    Seek(Seek),
    SetRate(f64),
    Raise,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seek {
    /// Seconds from the current position (negative: back). Seeking before the start goes to the
    /// start.
    By(f64),
    /// From the start. Positions past the end are ignored.
    To(Duration),
}

pub type Commands = async_channel::Sender<Command>;
