//! Keyboard shortcuts as gpui actions. [`SHORTCUTS`] is the single list: it's both bound at
//! startup and shown on the settings page.
//!
//! Key contexts: `Moth` is the whole window, `Watch` the watch page (which has focus while
//! watching; the address bar is outside it, so typing there never triggers watch keys).

use std::rc::Rc;

use gpui::{Action, App, KeyBinding, KeyBindingContextPredicate, actions};

use crate::settings::Settings;

actions!(
    moth,
    [
        FocusAddress,
        GoHome,
        OpenMusic,
        OpenSettings,
        /// Esc: close suggestions, leave the address bar, close a menu, or leave fullscreen.
        Cancel,
        TogglePause,
        SeekBack,
        SeekForward,
        SeekBackLong,
        SeekForwardLong,
        ToggleFullscreen,
        TogglePlayerStats,
        ToggleLayoutTargets,
        ToggleSlowMotion,
        CycleFrameOverlay,
    ]
);

pub const APP: &str = "Moth";
pub const WATCH: &str = "Watch";

pub struct Shortcut {
    /// Keystrokes in gpui's syntax.
    pub keys: &'static [&'static str],
    pub context: &'static str,
    pub action: fn() -> Box<dyn Action>,
    /// How the settings page lists it; `None` for debug keys.
    pub label: Option<&'static str>,
    pub describe: fn(&Settings) -> String,
}

macro_rules! shortcut {
    ($keys:expr, $context:expr, $action:expr, $label:expr, $describe:expr) => {
        Shortcut { keys: $keys, context: $context, action: || Box::new($action), label: $label, describe: $describe }
    };
}

pub const SHORTCUTS: &[Shortcut] = &[
    shortcut!(&["ctrl-l"], APP, FocusAddress, Some("Ctrl L"), |_| "Focus the search / address bar".into()),
    shortcut!(&["ctrl-1"], APP, GoHome, Some("Ctrl 1"), |_| "Home".into()),
    shortcut!(&["ctrl-2"], APP, OpenMusic, Some("Ctrl 2"), |_| "Music".into()),
    shortcut!(&["ctrl-,"], APP, OpenSettings, Some("Ctrl ,"), |_| "Open settings".into()),
    shortcut!(&["space", "k"], WATCH, TogglePause, Some("Space / K"), |_| "Play or pause".into()),
    shortcut!(&["j"], WATCH, SeekBackLong, Some("J / L"), |s| format!("Back / forward {} s", s.seek_step_long)),
    shortcut!(&["l"], WATCH, SeekForwardLong, None, |_| String::new()),
    shortcut!(&["left"], WATCH, SeekBack, Some("← / →"), |s| format!("Back / forward {} s", s.seek_step)),
    shortcut!(&["right"], WATCH, SeekForward, None, |_| String::new()),
    shortcut!(&["f"], WATCH, ToggleFullscreen, Some("F"), |_| "Toggle fullscreen (or double-click the video)".into()),
    shortcut!(&["escape"], APP, Cancel, Some("Esc"), |_| "Leave fullscreen, close a menu or the address bar".into()),
    shortcut!(&["f8"], WATCH, TogglePlayerStats, Some("F8"), |_| "Player debug panel".into()),
    shortcut!(&["f9"], APP, ToggleLayoutTargets, None, |_| String::new()),
    shortcut!(&["f10"], APP, ToggleSlowMotion, None, |_| String::new()),
    shortcut!(&["f12"], APP, CycleFrameOverlay, None, |_| String::new()),
];

pub fn bind(cx: &mut App) {
    let mut bindings = Vec::new();
    for shortcut in SHORTCUTS {
        let context = KeyBindingContextPredicate::parse(shortcut.context).ok().map(Rc::new);
        for keys in shortcut.keys {
            match KeyBinding::load(keys, (shortcut.action)(), context.clone(), false, None, cx) {
                Ok(binding) => bindings.push(binding),
                Err(err) => eprintln!("moth: invalid shortcut {keys:?}: {err}"),
            }
        }
    }
    cx.bind_keys(bindings);
}
