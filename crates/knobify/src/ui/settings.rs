//! Settings window: a deferred child viewport. Implemented by WP4.
//!
//! The root app shows it every pass while open; the viewport callback works on
//! the shared `SettingsState` and queues `SettingsAction`s that `App::logic`
//! drains and executes.

use std::sync::{Arc, Mutex};

use knobify_core::spotify::AuthState;
use knobify_core::{BindingTarget, Settings};

pub const VIEWPORT_TITLE: &str = "Knobify Settings";

pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("knobify-settings")
}

pub fn viewport_builder() -> egui::ViewportBuilder {
    let mut builder = egui::ViewportBuilder::default()
        .with_title(VIEWPORT_TITLE)
        .with_inner_size([440.0, 560.0])
        .with_min_inner_size([380.0, 420.0])
        .with_resizable(true);
    if let Ok(icon) = crate::icon::egui_icon() {
        builder = builder.with_icon(icon);
    }
    builder
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingsAction {
    /// Persist and apply the edited settings.
    Save(Settings),
    Login,
    CancelLogin,
    Logout,
    Capture(BindingTarget),
    CancelCapture,
    /// Show the popup with the current volume so the user can judge position/duration.
    PreviewOsd,
    Close,
}

#[derive(Debug, Clone)]
pub struct SettingsState {
    /// Unsaved edits.
    pub draft: Settings,
    /// Which binding is being captured (mirrors the hook's capture flag).
    pub capture: Option<BindingTarget>,
    pub auth: AuthState,
    /// `true` when the hook fell back to listen mode (suppression impossible).
    pub suppress_unavailable: bool,
    pub status_line: Option<String>,
    pub actions: Vec<SettingsAction>,
}

impl SettingsState {
    pub fn new(current: &Settings, auth: AuthState) -> Self {
        Self {
            draft: current.clone(),
            capture: None,
            auth,
            suppress_unavailable: false,
            status_line: None,
            actions: Vec::new(),
        }
    }
}

/// Body of the settings viewport. Called by the deferred viewport callback.
pub fn show(ui: &mut egui::Ui, state: &Arc<Mutex<SettingsState>>) {
    let Ok(mut state) = state.lock() else { return };
    ui.heading(VIEWPORT_TITLE);
    ui.label("Settings UI is implemented by WP4.");
    if ui.ctx().input(|i| i.viewport().close_requested()) {
        state.actions.push(SettingsAction::Close);
    }
}
