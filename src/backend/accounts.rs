//! Signed-in accounts, as the UI sees them. Sources with accounts (only YouTube so far) give
//! each [`Role`] its own sign-in, so one account can play and another get recommendations.

use std::time::Duration;

/// What an account is used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// Plays videos that need a signed-in viewer (age-restricted, members-only).
    Playback,
    /// Gets the home feed's recommendations, and the history of what's watched.
    Recommendations,
}

impl Role {
    pub const ALL: [Self; 2] = [Self::Playback, Self::Recommendations];

    /// Stable name, e.g. for keyring items and the IPC socket.
    pub fn key(self) -> &'static str {
        match self {
            Self::Playback => "playback",
            Self::Recommendations => "recommendations",
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.key() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Playback => "Playback account",
            Self::Recommendations => "Recommendation account",
        }
    }
}

/// How an account was signed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// A code entered on the site's device sign-in page (an OAuth token for a TV app).
    DeviceCode,
    /// A browser session's cookies.
    Cookies,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountInfo {
    /// e.g. the channel name, or "Signed in" until it's known.
    pub name: String,
    /// e.g. "@handle".
    pub handle: Option<String>,
    pub method: Method,
}

/// A device sign-in in progress: the user enters `code` at `url`.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceCode {
    pub code: String,
    pub url: String,
    pub expires_in: Duration,
}
