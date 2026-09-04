//! Thin Win32 helpers (windows-sys).
//!
//! * `apply_osd_exstyles`: add `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` so the
//!   popup stays out of Alt+Tab and never takes focus.
//! * `primary_work_area`: `SystemParametersInfoW(SPI_GETWORKAREA)` in physical
//!   pixels (excludes the taskbar).
//! * `set_rounded_corners`: `DwmSetWindowAttribute(DWMWA_WINDOW_CORNER_PREFERENCE)`
//!   for the opaque fallback on Windows 11.
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

#[cfg(windows)]
pub fn apply_osd_exstyles(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST,
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
    };

    let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;

    // SAFETY: single FFI call on a live HWND owned by this thread. `GWL_EXSTYLE`
    // reads the window's 32-bit extended style word and touches nothing else.
    let current = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    let wanted = current | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;

    // SAFETY: as above, but writing back the same style word. We only add bits,
    // so winit's own styles (WS_EX_TRANSPARENT | WS_EX_LAYERED for mouse
    // passthrough) are preserved.
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
