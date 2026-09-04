//! Thin Win32 helpers (windows-sys). Implemented by WP3.
//!
//! * `apply_osd_exstyles`: add `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` so the
//!   popup stays out of Alt+Tab and never takes focus.
//! * `primary_work_area`: `SystemParametersInfoW(SPI_GETWORKAREA)` in physical
//!   pixels (excludes the taskbar).
//! * `set_rounded_corners`: `DwmSetWindowAttribute(DWMWA_WINDOW_CORNER_PREFERENCE)`
//!   for the opaque fallback on Windows 11.

/// Physical-pixel rectangle: left, top, right, bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[cfg(windows)]
pub fn apply_osd_exstyles(hwnd: isize) {
    let _ = hwnd;
    todo!("WP3")
}

#[cfg(windows)]
pub fn primary_work_area() -> Option<Rect> {
    todo!("WP3")
}

#[cfg(windows)]
pub fn set_rounded_corners(hwnd: isize) {
    let _ = hwnd;
    todo!("WP3")
}

#[cfg(not(windows))]
pub fn apply_osd_exstyles(_hwnd: isize) {}

#[cfg(not(windows))]
pub fn primary_work_area() -> Option<Rect> {
    None
}

#[cfg(not(windows))]
pub fn set_rounded_corners(_hwnd: isize) {}
