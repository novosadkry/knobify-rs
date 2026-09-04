//! Events flowing from background threads into the UI thread, and the
//! user-facing action vocabulary shared by hotkeys, tray and settings.

use serde::{Deserialize, Serialize};

use crate::config::Bindings;
use crate::keycode::KeyCode;
use crate::spotify::SpotifyEvent;

/// What a bound key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HotkeyAction {
    VolumeUp,
    VolumeDown,
    MuteToggle,
}

/// Which binding the settings window is currently capturing a key for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingTarget {
    VolumeUp,
    VolumeDown,
    Mute,
}

impl BindingTarget {
    pub const ALL: [BindingTarget; 3] = [
        BindingTarget::VolumeUp,
        BindingTarget::VolumeDown,
        BindingTarget::Mute,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BindingTarget::VolumeUp => "Volume up",
            BindingTarget::VolumeDown => "Volume down",
            BindingTarget::Mute => "Mute / unmute",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrayAction {
    OpenSettings,
    LoginOrLogout,
    Exit,
}

/// Everything the UI thread reacts to. Producers: hotkey thread, tray
/// callbacks, Spotify actor.
#[derive(Debug, Clone)]
pub enum AppEvent {
    Hotkey(HotkeyAction),
    KeyCaptured { target: BindingTarget, key: KeyCode },
    CaptureCancelled,
    /// The global hook could not be installed in `grab` mode and fell back to
    /// passive listening; key suppression is unavailable.
    HookFallback(String),
    Tray(TrayAction),
    Spotify(SpotifyEvent),
}

/// Resolve a pressed key against the bindings.
pub fn match_key(bindings: &Bindings, key: &KeyCode) -> Option<HotkeyAction> {
    if *key == bindings.volume_up {
        Some(HotkeyAction::VolumeUp)
    } else if *key == bindings.volume_down {
        Some(HotkeyAction::VolumeDown)
    } else if bindings.mute.as_ref() == Some(key) {
        Some(HotkeyAction::MuteToggle)
    } else {
        None
    }
}
