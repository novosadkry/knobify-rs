//! Thin Win32 helpers (windows-sys).
//!
//! * `apply_osd_exstyles`: add `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` so the
//!   popup stays out of Alt+Tab and never takes focus.
//! * `primary_work_area`: `SystemParametersInfoW(SPI_GETWORKAREA)` in physical
//!   pixels (excludes the taskbar).
//! * `set_rounded_corners`: `DwmSetWindowAttribute(DWMWA_WINDOW_CORNER_PREFERENCE)`
//!   for the opaque fallback on Windows 11.
//! * `poll_pressed_key`: what key is being pressed right now, for rebinding.
//!
//! Every `unsafe` block is a single FFI call; the invariant it relies on is
//! stated above it. `hwnd` is always the root window's handle, obtained on the
//! UI thread (which owns the window) from winit via raw-window-handle, so it
//! is alive for the duration of the call.

/// Physical-pixel rectangle: left, top, right, bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    /// Width in physical pixels (never negative).
    pub fn width(&self) -> i32 {
        self.right.saturating_sub(self.left).max(0)
    }

    /// Height in physical pixels (never negative).
    pub fn height(&self) -> i32 {
        self.bottom.saturating_sub(self.top).max(0)
    }
}

/// The virtual-key code of a key that has just been pressed, if any.
///
/// Asks Windows directly (`GetAsyncKeyState`) rather than waiting for the
/// keyboard hook, so rebinding a key works even if the hook is not delivering:
/// the low bit of the result means "pressed since this was last called", which
/// is exactly the transition a rebind is waiting for.
///
/// Mouse buttons and modifiers are skipped: a knob is neither, and a keyboard
/// that emits its own modifier before the real key must not bind Shift.
#[cfg(windows)]
pub fn poll_pressed_key() -> Option<u32> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    /// Left/right mouse and the modifier codes, which are never a binding.
    const SKIP: &[u32] = &[
        0x01, 0x02, 0x04, 0x05, 0x06, 0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4,
        0xA5,
    ];

    (0x07..=0xFEu32).find(|vk| {
        if SKIP.contains(vk) {
            return false;
        }
        // SAFETY: single FFI call with an in-range virtual-key code.
        let state = unsafe { GetAsyncKeyState(*vk as i32) };
        // Bit 0: the key was pressed since the previous call for this key.
        state & 0x1 != 0
    })
}

#[cfg(not(windows))]
pub fn poll_pressed_key() -> Option<u32> {
    None
}

#[cfg(windows)]
pub fn apply_osd_exstyles(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST,
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_LAYERED, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };

    let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;

    // SAFETY: single FFI call on a live HWND owned by this thread. `GWL_EXSTYLE`
    // reads the window's 32-bit extended style word and touches nothing else.
    let current = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    // `WS_EX_TRANSPARENT` alone passes clicks through. `WS_EX_LAYERED` must be
    // cleared: layered compositing of the OpenGL surface fails on real
    // hardware, leaving a window that paints but is never seen.
    let wanted =
        (current | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT) & !WS_EX_LAYERED;

    // SAFETY: as above, but writing back the same style word.
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted as _);
    }

    // SAFETY: `SWP_NOMOVE | SWP_NOSIZE` means the x/y/cx/cy arguments are
    // ignored, so this only re-applies the frame (making the style change take
    // effect) and re-asserts the topmost z-order without activating the window.
    unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

#[cfg(windows)]
pub fn primary_work_area() -> Option<Rect> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWORKAREA};

    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };

    // SAFETY: `SPI_GETWORKAREA` writes exactly one `RECT` through `pvparam` and
    // reads neither `uiparam` nor `fwinini`; `rect` is a live, correctly typed
    // and aligned RECT that outlives the call.
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, std::ptr::from_mut(&mut rect).cast(), 0)
    };

    if ok == 0 || rect.right <= rect.left || rect.bottom <= rect.top {
        return None;
    }

    Some(Rect {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    })
}

#[cfg(windows)]
pub fn set_rounded_corners(hwnd: isize) {
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
        DWM_WINDOW_CORNER_PREFERENCE,
    };

    let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
    let preference: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;

    // SAFETY: `DWMWA_WINDOW_CORNER_PREFERENCE` expects a pointer to one
    // `DWM_WINDOW_CORNER_PREFERENCE` (4 bytes), which is exactly what we pass
    // together with its `size_of`. `preference` is a live local that outlives
    // the call. Unsupported attributes are reported as a failing HRESULT.
    let hr = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            std::ptr::from_ref(&preference).cast(),
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        )
    };

    if hr < 0 {
        // Pre-Windows 11 this attribute does not exist; rounded corners are a
        // nicety, so log and carry on.
        log::debug!("DwmSetWindowAttribute(DWMWA_WINDOW_CORNER_PREFERENCE) failed: {hr:#x}");
    }
}

#[cfg(not(windows))]
pub fn apply_osd_exstyles(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn primary_work_area() -> Option<Rect> {
    None
}

#[cfg(not(windows))]
pub fn set_rounded_corners(_hwnd: isize) {}
