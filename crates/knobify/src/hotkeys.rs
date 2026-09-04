//! Global keyboard hook (rdev) on a dedicated thread.
//!
//! Contract (implemented by WP2):
//! * Always `rdev::grab` (feature `listen-only` swaps in `rdev::listen`).
//!   Every event decides per key whether to swallow, so "suppress bound keys"
//!   and capture mode are live toggles without restarting the hook.
//! * The callback runs inside a low-level Windows hook and must return in
//!   microseconds: `try_read` the snapshot, match, `AppHandle::send`, return.
//! * Capture mode: when `capture` is `Some(target)`, the next `KeyPress` is
//!   reported as `AppEvent::KeyCaptured` and swallowed (Escape cancels).
//! * If installing the grab hook fails, fall back to `listen` and report
//!   `AppEvent::HookFallback`.

use std::sync::{Arc, Mutex, RwLock};

use knobify_core::{Bindings, BindingTarget, KeyCode};

use crate::handle::AppHandle;

#[derive(Debug, Default)]
pub struct HotkeyShared {
    pub bindings: RwLock<Bindings>,
    pub capture: Mutex<Option<BindingTarget>>,
}

/// UI-side controller for the hook thread.
#[derive(Debug, Clone)]
pub struct HotkeyController {
    shared: Arc<HotkeyShared>,
}

impl HotkeyController {
    pub fn set_bindings(&self, bindings: Bindings) {
        if let Ok(mut guard) = self.shared.bindings.write() {
            *guard = bindings;
        }
    }

    pub fn begin_capture(&self, target: BindingTarget) {
        if let Ok(mut guard) = self.shared.capture.lock() {
            *guard = Some(target);
        }
    }

    pub fn cancel_capture(&self) {
        if let Ok(mut guard) = self.shared.capture.lock() {
            *guard = None;
        }
    }

    pub fn is_capturing(&self) -> bool {
        self.shared
            .capture
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }
}

/// Start the hook thread. Never returns on its own; the process exit tears it down.
pub fn spawn_hotkey_thread(initial: Bindings, out: AppHandle) -> HotkeyController {
    let shared = Arc::new(HotkeyShared {
        bindings: RwLock::new(initial),
        capture: Mutex::new(None),
    });
    let controller = HotkeyController {
        shared: Arc::clone(&shared),
    };
    std::thread::Builder::new()
        .name("knobify-hotkeys".into())
        .spawn(move || run_hook(shared, out))
        .expect("spawning the hotkey thread cannot fail on Windows");
    controller
}

fn run_hook(shared: Arc<HotkeyShared>, out: AppHandle) {
    let _ = (shared, out);
    todo!("WP2: rdev::grab loop")
}

/// `rdev::Key` -> backend-independent code (`Unknown(vk)` -> `Raw`, else the variant name).
pub fn key_to_keycode(key: rdev::Key) -> KeyCode {
    let _ = key;
    todo!("WP2")
}
