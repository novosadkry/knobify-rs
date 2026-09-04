//! System tray icon and menu. Implemented by WP4.
//!
//! Must be created on the UI thread after the winit event loop exists (i.e.
//! inside the eframe app constructor). Menu/tray events are forwarded through
//! `AppHandle` as `AppEvent::Tray(..)`.

use knobify_core::spotify::AuthState;

use crate::handle::AppHandle;

pub struct TrayUi {
    _icon: tray_icon::TrayIcon,
}

impl TrayUi {
    pub fn new(handle: AppHandle) -> anyhow::Result<Self> {
        let _ = handle;
        todo!("WP4: build menu (Settings…, Login/Logout, Exit), register event handlers")
    }

    /// Update the login/logout label and tooltip.
    pub fn set_auth(&self, auth: &AuthState) {
        let _ = auth;
        todo!("WP4")
    }
}
