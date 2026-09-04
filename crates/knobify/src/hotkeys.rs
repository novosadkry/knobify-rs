//! Global keyboard hook (rdev) on a dedicated thread.
//!
//! Contract:
//! * Always `rdev::grab` (feature `listen-only` swaps in `rdev::listen`).
//!   Every event decides per key whether to swallow, so "suppress bound keys"
//!   and capture mode are live toggles without restarting the hook.
//! * The callback runs inside a low-level Windows hook and must return in
//!   microseconds: `try_read`/`try_lock` the snapshot, match, `AppHandle::send`, return.
//! * Capture mode: when `capture` is `Some(target)`, the next `KeyPress` is
//!   reported as `AppEvent::KeyCaptured` and swallowed (Escape cancels).
//! * If installing the grab hook fails, fall back to `listen` and report
//!   `AppEvent::HookFallback`.
//!
//! The decision logic (`decide`) is a pure function of the shared bindings /
//! capture state and one `rdev::Event`, so it is unit-tested directly without
//! ever installing a real OS hook.

use std::cell::Cell;
use std::sync::{Arc, Mutex, RwLock};

use knobify_core::events::match_key;
use knobify_core::{AppEvent, Bindings, BindingTarget, KeyCode};

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

    /// Not yet called from `app.rs` (reserved for a future settings-UI
    /// affordance, e.g. disabling other controls while capturing); part of
    /// the WP2 public API contract, exercised directly by this module's tests.
    #[allow(dead_code)]
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

/// What the hook callback should do with one event.
#[derive(Debug)]
struct Decision {
    emit: Option<AppEvent>,
    swallow: bool,
}

impl Decision {
    const PASS: Decision = Decision {
        emit: None,
        swallow: false,
    };
}

/// Pure decision logic shared by the `grab` and `listen` callbacks.
///
/// `swallow_release` is per-callback state (which physical key, if any, had
/// its press swallowed and therefore needs its matching release swallowed
/// too) — it lives in the hook thread's closure, never behind a lock.
fn decide(
    shared: &HotkeyShared,
    swallow_release: &Cell<Option<rdev::Key>>,
    event: &rdev::Event,
) -> Decision {
    match event.event_type {
        rdev::EventType::KeyPress(key) => on_key_press(shared, swallow_release, key),
        rdev::EventType::KeyRelease(key) => on_key_release(swallow_release, key),
        // Mouse move/click/wheel: never bound to anything, pass through with
        // zero work.
        _ => Decision::PASS,
    }
}

fn on_key_press(
    shared: &HotkeyShared,
    swallow_release: &Cell<Option<rdev::Key>>,
    key: rdev::Key,
) -> Decision {
    // `try_lock`: contended or poisoned is treated as "not capturing" so the
    // hook never blocks waiting on the UI thread.
    let captured_target = match shared.capture.try_lock() {
        Ok(mut guard) => guard.take(),
        Err(_) => None,
    };

    if let Some(target) = captured_target {
        swallow_release.set(Some(key));
        let emit = if key == rdev::Key::Escape {
            AppEvent::CaptureCancelled
        } else {
            AppEvent::KeyCaptured {
                target,
                key: key_to_keycode(key),
            }
        };
        return Decision {
            emit: Some(emit),
            swallow: true,
        };
    }

    let code = key_to_keycode(key);
    let bindings = match shared.bindings.try_read() {
        Ok(guard) => guard,
        Err(_) => return Decision::PASS,
    };

    match match_key(&bindings, &code) {
        Some(action) => {
            let swallow = bindings.suppress;
            if swallow {
                swallow_release.set(Some(key));
            }
            log::debug!("hotkey bound: {code} -> {action:?} (suppress={swallow})");
            Decision {
                emit: Some(AppEvent::Hotkey(action)),
                swallow,
            }
        }
        None => Decision::PASS,
    }
}

fn on_key_release(swallow_release: &Cell<Option<rdev::Key>>, key: rdev::Key) -> Decision {
    if swallow_release.get() == Some(key) {
        swallow_release.set(None);
        Decision {
            emit: None,
            swallow: true,
        }
    } else {
        Decision::PASS
    }
}

fn run_hook(shared: Arc<HotkeyShared>, out: AppHandle) {
    #[cfg(feature = "listen-only")]
    {
        out.send(AppEvent::HookFallback("built with listen-only".into()));
        run_listen(shared, out);
    }

    #[cfg(not(feature = "listen-only"))]
    {
        run_grab(shared, out);
    }
}

#[cfg(not(feature = "listen-only"))]
fn run_grab(shared: Arc<HotkeyShared>, out: AppHandle) {
    let grab_shared = Arc::clone(&shared);
    let grab_out = out.clone();
    let swallow_release = Cell::new(None);
    let callback = move |event: rdev::Event| -> Option<rdev::Event> {
        let decision = decide(&grab_shared, &swallow_release, &event);
        if let Some(app_event) = decision.emit {
            grab_out.send(app_event);
        }
        if decision.swallow {
            None
        } else {
            Some(event)
        }
    };

    if let Err(e) = rdev::grab(callback) {
        out.send(AppEvent::HookFallback(format!(
            "installing the low-level keyboard hook failed ({e:?}); \
             falling back to a passive listener (key suppression unavailable)"
        )));
        run_listen(shared, out);
    }
}

fn run_listen(shared: Arc<HotkeyShared>, out: AppHandle) {
    let swallow_release = Cell::new(None);
    let callback = move |event: rdev::Event| {
        let decision = decide(&shared, &swallow_release, &event);
        if let Some(app_event) = decision.emit {
            out.send(app_event);
        }
        // `listen` cannot swallow events; `decision.swallow` only matters for
        // the release-tracking state above.
    };
    if let Err(e) = rdev::listen(callback) {
        log::error!("rdev::listen failed to install the low-level hooks: {e:?}");
    }
}

/// `rdev::Key` -> backend-independent code (`Unknown(vk)` -> `Raw`, else the variant name).
pub fn key_to_keycode(key: rdev::Key) -> KeyCode {
    match key {
        rdev::Key::Unknown(code) => KeyCode::Raw(code),
        other => KeyCode::Named(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use knobify_core::HotkeyAction;

    use super::*;

    fn shared_with(bindings: Bindings) -> HotkeyShared {
        HotkeyShared {
            bindings: RwLock::new(bindings),
            capture: Mutex::new(None),
        }
    }

    fn key_event(event_type: rdev::EventType) -> rdev::Event {
        rdev::Event {
            time: SystemTime::now(),
            name: None,
            event_type,
        }
    }

    fn suppressing_bindings() -> Bindings {
        Bindings {
            volume_up: KeyCode::Raw(0x82),
            volume_down: KeyCode::Raw(0x81),
            mute: None,
            suppress: true,
        }
    }

    #[test]
    fn bound_key_with_suppress_swallows_and_emits_hotkey() {
        let shared = shared_with(suppressing_bindings());
        let swallow_release = Cell::new(None);
        let event = key_event(rdev::EventType::KeyPress(rdev::Key::Unknown(0x82)));

        let decision = decide(&shared, &swallow_release, &event);

        assert!(decision.swallow);
        assert!(matches!(
            decision.emit,
            Some(AppEvent::Hotkey(HotkeyAction::VolumeUp))
        ));
        // The press was swallowed, so the matching release must be too.
        assert_eq!(swallow_release.get(), Some(rdev::Key::Unknown(0x82)));
    }

    #[test]
    fn bound_key_without_suppress_passes_through_and_emits_hotkey() {
        let mut bindings = suppressing_bindings();
        bindings.suppress = false;
        let shared = shared_with(bindings);
        let swallow_release = Cell::new(None);
        let event = key_event(rdev::EventType::KeyPress(rdev::Key::Unknown(0x81)));

        let decision = decide(&shared, &swallow_release, &event);

        assert!(!decision.swallow);
        assert!(matches!(
            decision.emit,
            Some(AppEvent::Hotkey(HotkeyAction::VolumeDown))
        ));
        assert_eq!(swallow_release.get(), None);
    }

    #[test]
    fn unbound_key_passes_through_with_no_event() {
        let shared = shared_with(suppressing_bindings());
        let swallow_release = Cell::new(None);
        let event = key_event(rdev::EventType::KeyPress(rdev::Key::KeyA));

        let decision = decide(&shared, &swallow_release, &event);

        assert!(!decision.swallow);
        assert!(decision.emit.is_none());
    }

    #[test]
    fn capture_mode_reports_key_captured_and_swallows() {
        let shared = shared_with(Bindings::default());
        *shared.capture.lock().unwrap() = Some(BindingTarget::Mute);
        let swallow_release = Cell::new(None);
        let event = key_event(rdev::EventType::KeyPress(rdev::Key::Function));

        let decision = decide(&shared, &swallow_release, &event);

        assert!(decision.swallow);
        match decision.emit {
            Some(AppEvent::KeyCaptured { target, key }) => {
                assert_eq!(target, BindingTarget::Mute);
                assert_eq!(key, KeyCode::named("Function"));
            }
            other => panic!("expected KeyCaptured, got {other:?}"),
        }
        // Capture is one-shot: the slot must be cleared.
        assert!(shared.capture.lock().unwrap().is_none());
        assert_eq!(swallow_release.get(), Some(rdev::Key::Function));
    }

    #[test]
    fn capture_mode_escape_cancels() {
        let shared = shared_with(Bindings::default());
        *shared.capture.lock().unwrap() = Some(BindingTarget::VolumeUp);
        let swallow_release = Cell::new(None);
        let event = key_event(rdev::EventType::KeyPress(rdev::Key::Escape));

        let decision = decide(&shared, &swallow_release, &event);

        assert!(decision.swallow);
        assert!(matches!(decision.emit, Some(AppEvent::CaptureCancelled)));
        assert!(shared.capture.lock().unwrap().is_none());
    }

    #[test]
    fn release_after_swallowed_press_is_swallowed_once() {
        let shared = shared_with(suppressing_bindings());
        let swallow_release = Cell::new(None);
        let press = key_event(rdev::EventType::KeyPress(rdev::Key::Unknown(0x82)));
        let release = key_event(rdev::EventType::KeyRelease(rdev::Key::Unknown(0x82)));

        let press_decision = decide(&shared, &swallow_release, &press);
        assert!(press_decision.swallow);

        let release_decision = decide(&shared, &swallow_release, &release);
        assert!(release_decision.swallow);
        assert!(release_decision.emit.is_none());
        assert_eq!(swallow_release.get(), None);

        // A second release (or a release of some other key) is not swallowed.
        let release_decision2 = decide(&shared, &swallow_release, &release);
        assert!(!release_decision2.swallow);
    }

    #[test]
    fn mouse_events_always_pass_through() {
        let shared = shared_with(suppressing_bindings());
        let swallow_release = Cell::new(None);

        for event_type in [
            rdev::EventType::MouseMove { x: 1.0, y: 2.0 },
            rdev::EventType::ButtonPress(rdev::Button::Left),
            rdev::EventType::ButtonRelease(rdev::Button::Left),
            rdev::EventType::Wheel {
                delta_x: 0,
                delta_y: 1,
            },
        ] {
            let event = key_event(event_type);
            let decision = decide(&shared, &swallow_release, &event);
            assert!(!decision.swallow);
            assert!(decision.emit.is_none());
        }
    }

    #[test]
    fn emitted_events_reach_the_app_handle() {
        let (handle, rx) = AppHandle::new(egui::Context::default());
        let shared = shared_with(suppressing_bindings());
        let swallow_release = Cell::new(None);
        let event = key_event(rdev::EventType::KeyPress(rdev::Key::Unknown(0x82)));

        let decision = decide(&shared, &swallow_release, &event);
        if let Some(app_event) = decision.emit {
            handle.send(app_event);
        }

        let received = rx.try_recv().expect("expected an emitted AppEvent");
        assert!(matches!(
            received,
            AppEvent::Hotkey(HotkeyAction::VolumeUp)
        ));
    }

    #[test]
    fn key_to_keycode_maps_unknown_to_raw_and_named_otherwise() {
        assert_eq!(key_to_keycode(rdev::Key::Unknown(0x82)), KeyCode::Raw(0x82));
        assert_eq!(
            key_to_keycode(rdev::Key::Function),
            KeyCode::named("Function")
        );
    }

    #[test]
    fn controller_bindings_and_capture_round_trip() {
        let (handle, _rx) = AppHandle::new(egui::Context::default());
        let controller = spawn_disabled_controller();

        assert!(!controller.is_capturing());
        controller.begin_capture(BindingTarget::VolumeUp);
        assert!(controller.is_capturing());
        controller.cancel_capture();
        assert!(!controller.is_capturing());

        let bindings = Bindings {
            volume_up: KeyCode::named("Function"),
            ..Bindings::default()
        };
        controller.set_bindings(bindings.clone());
        assert_eq!(*controller.shared.bindings.read().unwrap(), bindings);

        let _ = handle;
    }

    /// A `HotkeyController` over shared state with no hook thread attached —
    /// exercises the controller API without touching rdev.
    fn spawn_disabled_controller() -> HotkeyController {
        HotkeyController {
            shared: Arc::new(HotkeyShared::default()),
        }
    }
}
