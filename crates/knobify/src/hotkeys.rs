//! The global keyboard hook, on a dedicated thread.
//!
//! # Why this is written defensively
//!
//! A `WH_KEYBOARD_LL` callback that overruns `LowLevelHooksTimeout` (300 ms by
//! default) is removed by Windows silently and permanently: no error, no
//! further calls, for the rest of the process. This app lost its hook that way
//! twice - first through rdev, whose callback calls `AttachThreadInput` against
//! the foreground window's thread before handing the event over, then through
//! logging and an `egui` repaint request in the callback, both of which can
//! block on a lock the UI thread holds while it renders.
//!
//! So two rules hold here, and the hook is treated as something that can fail
//! at any time rather than something that stays installed:
//!
//! * The callback does nothing but read two locks with `try_*`, compare a
//!   number, and queue the event through [`HookSender`] (a channel send plus a
//!   lock-free `unpark`). No logging, no egui, no allocation beyond the queue.
//! * The hook is re-installed every few seconds. Reinstalling costs two
//!   syscalls, and it means anything that takes the hook down - including
//!   causes not yet understood - costs one interval of dead keys instead of the
//!   whole session.
//!
//! Rebinding deliberately does **not** go through here: `win::poll_pressed_key`
//! asks Windows for the key directly, so capturing a new binding works even
//! when the hook does not.
//!
//! Returning `1` from the callback swallows the key so nothing else on the
//! system sees it; that happens only for a bound key while `bindings.suppress`
//! is on, and a swallowed press must have its release swallowed too, or
//! applications see a key that was never pressed.

use std::cell::Cell;
use std::sync::{Arc, RwLock};

use knobify_core::events::match_key;
use knobify_core::{AppEvent, Bindings, KeyCode};

use crate::handle::{AppHandle, HookSender};

/// How often the hook is torn down and installed again.
#[cfg(windows)]
const REARM_INTERVAL_MS: u32 = 5_000;

#[derive(Debug, Default)]
pub struct HotkeyShared {
    pub bindings: RwLock<Bindings>,
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
}

/// Whether a key event is a press or a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyKind {
    Down,
    Up,
}

/// What the hook should do with one key event.
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

/// Start the hook thread. It runs for the life of the process: a low-level hook
/// belongs to the thread that installed it and dies with it.
pub fn spawn_hotkey_thread(initial: Bindings, out: AppHandle) -> HotkeyController {
    let shared = Arc::new(HotkeyShared {
        bindings: RwLock::new(initial),
    });
    let controller = HotkeyController {
        shared: Arc::clone(&shared),
    };
    // Built here, on the UI thread, so the hook thread never has to: it also
    // starts the waker thread the callback pokes.
    let hook_out = HookSender::new(&out);
    if let Err(e) = std::thread::Builder::new()
        .name("knobify-hotkeys".into())
        .spawn(move || run_hook(shared, out, hook_out))
    {
        // The tray, Settings and the popup still work, so stay up and say why.
        log::error!("cannot start the hotkey thread ({e}); bound keys will not work");
    }
    controller
}

/// Decide what to do with one key event. Pure, so it can be tested without a
/// hook: `swallow_release` is the callback's own state, not shared with anyone.
fn decide(
    shared: &HotkeyShared,
    swallow_release: &Cell<Option<u32>>,
    vk: u32,
    kind: KeyKind,
) -> Decision {
    match kind {
        KeyKind::Up => {
            if swallow_release.get() == Some(vk) {
                swallow_release.set(None);
                Decision {
                    emit: None,
                    swallow: true,
                }
            } else {
                Decision::PASS
            }
        }
        KeyKind::Down => {
            // `try_read`: contended or poisoned counts as "not bound", which
            // loses one keystroke rather than the whole hook.
            let Ok(bindings) = shared.bindings.try_read() else {
                return Decision::PASS;
            };
            match match_key(&bindings, &KeyCode::from_vk(vk)) {
                Some(action) => {
                    let swallow = bindings.suppress;
                    if swallow {
                        swallow_release.set(Some(vk));
                    }
                    Decision {
                        emit: Some(AppEvent::Hotkey(action)),
                        swallow,
                    }
                }
                None => Decision::PASS,
            }
        }
    }
}

#[cfg(windows)]
fn run_hook(shared: Arc<HotkeyShared>, out: AppHandle, hook_out: HookSender) {
    use std::cell::RefCell;
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION,
        HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
        WM_SYSKEYUP, WM_TIMER,
    };

    /// Everything the callback needs. It lives in a thread local because a
    /// low-level hook callback always runs on the thread that installed the
    /// hook, which sidesteps sharing it (the channel sender inside
    /// `HookSender` is `Send` but not `Sync`).
    struct HookState {
        shared: Arc<HotkeyShared>,
        out: HookSender,
        swallow_release: Cell<Option<u32>>,
    }

    thread_local! {
        static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let kind = match wparam as u32 {
                WM_KEYDOWN | WM_SYSKEYDOWN => Some(KeyKind::Down),
                WM_KEYUP | WM_SYSKEYUP => Some(KeyKind::Up),
                _ => None,
            };
            if let Some(kind) = kind {
                // SAFETY: for HC_ACTION on a keyboard hook, Windows guarantees
                // `lparam` points at a `KBDLLHOOKSTRUCT` valid for this call.
                let vk = unsafe { (*(lparam as *const KBDLLHOOKSTRUCT)).vkCode };
                let swallow = STATE.with(|state| {
                    let state = state.borrow();
                    let Some(state) = state.as_ref() else {
                        return false;
                    };
                    let decision = decide(&state.shared, &state.swallow_release, vk, kind);
                    if let Some(event) = decision.emit {
                        state.out.send(event);
                    }
                    decision.swallow
                });
                if swallow {
                    // Non-zero: the key goes no further, not even to the
                    // foreground application.
                    return 1;
                }
            }
        }
        // SAFETY: single FFI call. The first argument is ignored for low-level
        // hooks, which is why the handle does not need keeping.
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    /// SAFETY of the calls inside: a null module handle with thread id 0 is how
    /// a global low-level hook is installed, and `hook_proc` is `'static`.
    fn install() -> Option<HHOOK> {
        let hook =
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), std::ptr::null_mut(), 0) };
        if hook.is_null() {
            None
        } else {
            Some(hook)
        }
    }

    STATE.with(|state| {
        *state.borrow_mut() = Some(HookState {
            shared,
            out: hook_out,
            swallow_release: Cell::new(None),
        });
    });

    let Some(mut hook) = install() else {
        let error = std::io::Error::last_os_error();
        log::error!("could not install the keyboard hook: {error}");
        out.send(AppEvent::HookFallback(format!(
            "Windows refused the keyboard hook ({error}); bound keys will not work"
        )));
        return;
    };
    log::info!("global key hook installed (bound keys can be swallowed)");

    // A thread timer, so the message loop below wakes up on its own.
    // SAFETY: a null window with a non-zero id creates a thread timer whose
    // WM_TIMER arrives in this thread's queue.
    unsafe { SetTimer(std::ptr::null_mut(), 1, REARM_INTERVAL_MS, None) };

    // The hook is only dispatched while this thread retrieves messages, and
    // Windows destroys it when the thread exits - so this loop is the hook's
    // life, and it also re-arms it (see the module docs).
    // SAFETY: `msg` is a live local; a null window means "any message".
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if result <= 0 {
            // 0 is WM_QUIT and -1 an error; both are sticky, so returning here
            // would drop the hook and looping straight back would spin a core.
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }
        if msg.message == WM_TIMER {
            unsafe { UnhookWindowsHookEx(hook) };
            match install() {
                Some(fresh) => hook = fresh,
                None => {
                    let error = std::io::Error::last_os_error();
                    log::error!("could not re-arm the keyboard hook: {error}");
                    // Try again on the next tick rather than giving up.
                }
            }
        }
    }
}

#[cfg(not(windows))]
fn run_hook(_shared: Arc<HotkeyShared>, out: AppHandle, _hook_out: HookSender) {
    out.send(AppEvent::HookFallback(
        "global hotkeys are only implemented for Windows".to_owned(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use knobify_core::HotkeyAction;

    /// F13 and F15: what the knob this was written for actually sends.
    const VK_F13: u32 = 0x7C;
    const VK_F15: u32 = 0x7E;
    const VK_A: u32 = 0x41;

    fn shared_with(bindings: Bindings) -> HotkeyShared {
        HotkeyShared {
            bindings: RwLock::new(bindings),
        }
    }

    fn knob_bindings(suppress: bool) -> Bindings {
        Bindings {
            volume_up: KeyCode::Raw(VK_F13),
            volume_down: KeyCode::Raw(VK_F15),
            mute: None,
            suppress,
        }
    }

    #[test]
    fn bound_key_emits_its_action_and_passes_through_by_default() {
        let shared = shared_with(knob_bindings(false));
        let swallow_release = Cell::new(None);

        let decision = decide(&shared, &swallow_release, VK_F13, KeyKind::Down);

        assert!(
            !decision.swallow,
            "suppress is off, Windows must still see it"
        );
        assert!(matches!(
            decision.emit,
            Some(AppEvent::Hotkey(HotkeyAction::VolumeUp))
        ));
        assert_eq!(swallow_release.get(), None);
    }

    #[test]
    fn bound_key_is_swallowed_when_suppress_is_on() {
        let shared = shared_with(knob_bindings(true));
        let swallow_release = Cell::new(None);

        let decision = decide(&shared, &swallow_release, VK_F15, KeyKind::Down);

        assert!(decision.swallow);
        assert!(matches!(
            decision.emit,
            Some(AppEvent::Hotkey(HotkeyAction::VolumeDown))
        ));
        // The release has to go too, or apps see a key that was never pressed.
        assert_eq!(swallow_release.get(), Some(VK_F15));
        let release = decide(&shared, &swallow_release, VK_F15, KeyKind::Up);
        assert!(release.swallow);
        assert_eq!(swallow_release.get(), None);
        // ... but only once.
        assert!(!decide(&shared, &swallow_release, VK_F15, KeyKind::Up).swallow);
    }

    #[test]
    fn a_named_binding_matches_the_same_physical_key() {
        let shared = shared_with(Bindings {
            volume_up: KeyCode::named("F13"),
            ..knob_bindings(false)
        });
        let swallow_release = Cell::new(None);

        let decision = decide(&shared, &swallow_release, VK_F13, KeyKind::Down);

        assert!(matches!(
            decision.emit,
            Some(AppEvent::Hotkey(HotkeyAction::VolumeUp))
        ));
    }

    #[test]
    fn unbound_key_passes_through_with_no_event() {
        let shared = shared_with(knob_bindings(true));
        let swallow_release = Cell::new(None);

        let decision = decide(&shared, &swallow_release, VK_A, KeyKind::Down);

        assert!(!decision.swallow);
        assert!(decision.emit.is_none());
    }
}
