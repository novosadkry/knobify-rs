//! Settings window: a deferred child viewport.
//!
//! The root app shows it every pass while open; the viewport callback works on
//! the shared `SettingsState` and queues `SettingsAction`s that `App::logic`
//! drains and executes. `show` never blocks and never touches I/O: it only
//! reads/writes the locked `SettingsState`.

use std::sync::{Arc, Mutex};

use knobify_core::config::{
    MAX_OSD_DURATION_MS, MAX_STEP, MAX_SYNC_DELAY_MS, MIN_OSD_DURATION_MS, MIN_STEP,
    MIN_SYNC_DELAY_MS,
};
use knobify_core::spotify::AuthState;
use knobify_core::{BindingTarget, KeyCode, OsdPosition, Settings};

pub const VIEWPORT_TITLE: &str = "Knobify Settings";

const MIN_REDIRECT_PORT: u16 = 1024;
const MAX_REDIRECT_PORT: u16 = 65535;
const MAX_OSD_MARGIN: f32 = 400.0;

const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(224, 90, 90);
const HINT_COLOR: egui::Color32 = egui::Color32::from_rgb(120, 170, 240);

/// Status line on first run, when no client ID has been entered yet.
pub const SETUP_HINT: &str = "Paste your Spotify Client ID, then Log in.";
/// The longer version, wrapped inside the Spotify section.
const SETUP_STEPS: &str = "First run: create an app at developer.spotify.com/dashboard, \
     register the redirect URI below in it, then paste its Client ID here.";

pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("knobify-settings")
}

pub fn viewport_builder() -> egui::ViewportBuilder {
    let mut builder = egui::ViewportBuilder::default()
        .with_title(VIEWPORT_TITLE)
        .with_inner_size([440.0, 560.0])
        .with_min_inner_size([380.0, 420.0])
        .with_resizable(true);
    if let Some(icon) = crate::icon::egui_icon() {
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
    /// The most recently saved (or loaded) settings; `draft != saved` enables Save.
    pub saved: Settings,
    /// Which binding is being captured (mirrors the hook's capture flag).
    pub capture: Option<BindingTarget>,
    pub auth: AuthState,
    /// `true` when the hook fell back to listen mode (suppression impossible).
    pub suppress_unavailable: bool,
    /// One line under the buttons: a save error, a confirmation, or the
    /// first-run hint. Cleared as soon as the user edits anything.
    pub status_line: Option<String>,
    /// Paint [`Self::status_line`] as an error rather than a hint.
    pub status_is_error: bool,
    pub actions: Vec<SettingsAction>,
}

impl SettingsState {
    pub fn new(current: &Settings, auth: AuthState) -> Self {
        let needs_setup = current.client_id.trim().is_empty();
        Self {
            draft: current.clone(),
            saved: current.clone(),
            capture: None,
            auth,
            suppress_unavailable: false,
            status_line: needs_setup.then(|| SETUP_HINT.to_owned()),
            status_is_error: false,
            actions: Vec::new(),
        }
    }

    /// Report a failure under the buttons.
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.status_line = Some(message.into());
        self.status_is_error = true;
    }

    /// Report a non-failure (a confirmation, a hint) under the buttons.
    pub fn set_hint(&mut self, message: impl Into<String>) {
        self.status_line = Some(message.into());
        self.status_is_error = false;
    }
}

/// Body of the settings viewport. Called by the deferred viewport callback.
pub fn show(ui: &mut egui::Ui, state: &Arc<Mutex<SettingsState>>) {
    let Ok(mut guard) = state.lock() else { return };
    let state = &mut *guard;
    let queued = state.actions.len();

    if ui.ctx().input(|i| i.viewport().close_requested()) {
        state.actions.push(SettingsAction::Close);
    }
    if state.capture.is_some() && ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        // The hook thread also watches for Escape; a second CancelCapture is harmless.
        state.actions.push(SettingsAction::CancelCapture);
    }

    let bottom_bar_height = 40.0;
    let scroll_height = (ui.available_height() - bottom_bar_height).max(100.0);

    egui::ScrollArea::vertical()
        .max_height(scroll_height)
        .show(ui, |ui| {
            show_spotify_section(ui, state);
            ui.add_space(10.0);
            show_bindings_section(ui, state);
            ui.add_space(10.0);
            show_behaviour_section(ui, state);
            ui.add_space(10.0);
            show_popup_section(ui, state);
        });

    ui.separator();
    show_bottom_bar(ui, state);

    if state.actions.len() != queued {
        // Only the root viewport runs `App::logic`, which is what executes
        // these actions; repainting this viewport alone would leave every
        // button dead until something else woke the root.
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
    }
}

fn show_spotify_section(ui: &mut egui::Ui, state: &mut SettingsState) {
    ui.heading("Spotify");
    ui.group(|ui| {
        if state.draft.client_id.trim().is_empty() {
            ui.label(egui::RichText::new(SETUP_STEPS).color(HINT_COLOR).small());
            ui.add_space(4.0);
        }

        match state.auth.clone() {
            AuthState::LoggedOut => {
                ui.label("Status: logged out.");
            }
            AuthState::LoggingIn { authorize_url } => {
                ui.label("Status: logging in — finish sign-in in your browser.");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(authorize_url.as_str()).monospace())
                            .selectable(true)
                            .truncate(),
                    );
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(authorize_url.clone());
                    }
                });
            }
            AuthState::LoggedIn { display_name } => {
                let name = display_name.unwrap_or_else(|| "your account".to_owned());
                ui.label(format!("Status: logged in as {name}."));
            }
            AuthState::Failed(message) => {
                ui.colored_label(ERROR_COLOR, format!("Status: login failed — {message}"));
            }
        }

        ui.add_space(4.0);
        egui::Grid::new("spotify_grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Client ID");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut state.draft.client_id)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(220.0),
                );
                if response.changed() {
                    state.draft.client_id = state.draft.client_id.trim().to_owned();
                    state.status_line = None;
                }
                ui.end_row();

                ui.label("Redirect URI");
                ui.horizontal(|ui| {
                    let mut uri = state.draft.redirect_uri();
                    ui.add_enabled(
                        false,
                        egui::TextEdit::singleline(&mut uri).desired_width(200.0),
                    );
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(uri);
                    }
                });
                ui.end_row();

                ui.label("Redirect port");
                let response = ui.add(
                    egui::DragValue::new(&mut state.draft.redirect_port)
                        .range(MIN_REDIRECT_PORT..=MAX_REDIRECT_PORT),
                );
                if response.changed() {
                    state.status_line = None;
                }
                ui.end_row();
            });
        ui.small("Register this exact URI in your Spotify app (developer.spotify.com/dashboard).");

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let logging_in = matches!(state.auth, AuthState::LoggingIn { .. });
            let logged_in = state.auth.is_logged_in();
            if logging_in {
                if ui.button("Cancel").clicked() {
                    state.actions.push(SettingsAction::CancelLogin);
                }
            } else if logged_in {
                if ui.button("Log out").clicked() {
                    state.actions.push(SettingsAction::Logout);
                }
            } else {
                let enabled = !state.draft.client_id.trim().is_empty();
                if ui
                    .add_enabled(enabled, egui::Button::new("Log in"))
                    .clicked()
                {
                    state.actions.push(SettingsAction::Login);
                }
            }
        });
        ui.small("Controlling playback requires Spotify Premium.");
    });
}

fn show_bindings_section(ui: &mut egui::Ui, state: &mut SettingsState) {
    ui.heading("Knob bindings");
    ui.group(|ui| {
        let any_capturing = state.capture.is_some();
        egui::Grid::new("bindings_grid")
            .num_columns(3)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                for target in BindingTarget::ALL {
                    let current = current_binding(&state.draft, target);
                    let is_capturing_this = state.capture == Some(target);

                    ui.label(target.label());
                    ui.label(optional_binding_label(&current));

                    ui.horizontal(|ui| {
                        if is_capturing_this {
                            if ui.button("Press a key…").clicked() {
                                state.actions.push(SettingsAction::CancelCapture);
                            }
                            ui.small("(Esc to cancel)");
                        } else {
                            let enabled = !any_capturing;
                            if ui
                                .add_enabled(enabled, egui::Button::new("Rebind"))
                                .clicked()
                            {
                                state.actions.push(SettingsAction::Capture(target));
                            }
                            if target == BindingTarget::Mute
                                && ui
                                    .add_enabled(enabled, egui::Button::new("Clear"))
                                    .clicked()
                            {
                                state.draft.bindings.mute = None;
                                state.status_line = None;
                            }
                        }
                    });
                    ui.end_row();
                }
            });
        ui.small("Knobs usually send a function key above F12, such as \"F13\".");
    });
}

fn current_binding(settings: &Settings, target: BindingTarget) -> Option<KeyCode> {
    match target {
        BindingTarget::VolumeUp => Some(settings.bindings.volume_up.clone()),
        BindingTarget::VolumeDown => Some(settings.bindings.volume_down.clone()),
        BindingTarget::Mute => settings.bindings.mute.clone(),
    }
}

fn show_behaviour_section(ui: &mut egui::Ui, state: &mut SettingsState) {
    ui.heading("Behaviour");
    ui.group(|ui| {
        let response = ui.add(
            egui::Slider::new(&mut state.draft.step, MIN_STEP..=MAX_STEP).text("Step per tick, %"),
        );
        if response.changed() {
            state.status_line = None;
        }

        let response = ui
            .add(
                egui::Slider::new(
                    &mut state.draft.sync_delay_ms,
                    MIN_SYNC_DELAY_MS..=MAX_SYNC_DELAY_MS,
                )
                .text("Spotify sync delay, ms"),
            )
            .on_hover_text(
                "Spotify reports a volume change one to three seconds after accepting it.                  Inside this window Knobify trusts its own value and ignores Spotify's;                  after it, Spotify is read again before the knob works from it.",
            );
        if response.changed() {
            state.status_line = None;
        }
        ui.small("Raise this if a turn of the knob gets pulled back to an older value.");

        let suppress_disabled = state.suppress_unavailable;
        ui.add_enabled_ui(!suppress_disabled, |ui| {
            let response = ui.checkbox(
                &mut state.draft.bindings.suppress,
                "Swallow bound keys (they never reach Windows or other apps)",
            );
            if response.changed() {
                state.status_line = None;
            }
        });
        if suppress_disabled {
            ui.small("Unavailable: Windows refused the global key hook on this system.");
        }
    });
}

fn show_popup_section(ui: &mut egui::Ui, state: &mut SettingsState) {
    ui.heading("Popup");
    ui.group(|ui| {
        let response = ui.checkbox(&mut state.draft.osd.enabled, "Show the volume popup");
        if response.changed() {
            state.status_line = None;
        }

        ui.add_enabled_ui(state.draft.osd.enabled, |ui| {
            egui::Grid::new("popup_grid")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Duration");
                    ui.add(
                        egui::Slider::new(
                            &mut state.draft.osd.duration_ms,
                            MIN_OSD_DURATION_MS..=MAX_OSD_DURATION_MS,
                        )
                        .suffix(" ms"),
                    );
                    ui.end_row();

                    ui.label("Position");
                    egui::ComboBox::from_id_salt("osd_position")
                        .selected_text(state.draft.osd.position.label())
                        .show_ui(ui, |ui| {
                            for position in OsdPosition::ALL {
                                ui.selectable_value(
                                    &mut state.draft.osd.position,
                                    position,
                                    position.label(),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label("Margin");
                    ui.add(
                        egui::DragValue::new(&mut state.draft.osd.margin)
                            .range(0.0..=MAX_OSD_MARGIN)
                            .suffix(" pt"),
                    );
                    ui.end_row();
                });

            ui.checkbox(&mut state.draft.osd.transparent, "Transparent background");
            ui.small("Takes effect after restart. Turn off if the popup renders as a black box.");

            if ui.button("Preview").clicked() {
                state.actions.push(SettingsAction::PreviewOsd);
            }
        });
    });
}

fn show_bottom_bar(ui: &mut egui::Ui, state: &mut SettingsState) {
    ui.horizontal(|ui| {
        let dirty = state.draft != state.saved;
        if ui.add_enabled(dirty, egui::Button::new("Save")).clicked() {
            // `App::apply_settings` writes the file and then puts the sanitized
            // settings back into `draft`/`saved`, so a failed save leaves the
            // Save button enabled.
            state
                .actions
                .push(SettingsAction::Save(state.draft.clone()));
        }
        if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
            state.draft = state.saved.clone();
            state.status_line = None;
        }
        if ui.button("Close").clicked() {
            state.actions.push(SettingsAction::Close);
        }
        if let Some(status) = &state.status_line {
            let color = if state.status_is_error {
                ERROR_COLOR
            } else {
                HINT_COLOR
            };
            ui.label(egui::RichText::new(status).color(color).small());
        }
    });
}

/// Human-readable label for an optional binding: `"Not set"` when absent.
fn optional_binding_label(key: &Option<KeyCode>) -> String {
    match key {
        Some(key) => key.to_string(),
        None => "Not set".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_binding_label_reports_not_set() {
        assert_eq!(optional_binding_label(&None), "Not set");
    }

    #[test]
    fn optional_binding_label_uses_keycode_display() {
        let key = Some(KeyCode::named("VolumeUp"));
        assert_eq!(optional_binding_label(&key), "Volume Up");

        // 0x82 is VK_F19, so it gets the name rather than the number.
        let f19 = Some(KeyCode::raw(0x82));
        assert_eq!(optional_binding_label(&f19), "F19");

        // A code Windows does not name falls back to the number.
        let raw = Some(KeyCode::raw(0x07));
        assert_eq!(optional_binding_label(&raw), "Key 0x07");
    }

    #[test]
    fn a_missing_client_id_shows_the_setup_hint_not_an_error() {
        let state = SettingsState::new(&Settings::default(), AuthState::LoggedOut);
        assert_eq!(state.status_line.as_deref(), Some(SETUP_HINT));
        assert!(!state.status_is_error);

        let configured = Settings {
            client_id: "abc123".to_owned(),
            ..Settings::default()
        };
        let state = SettingsState::new(&configured, AuthState::LoggedOut);
        assert_eq!(state.status_line, None);
    }

    #[test]
    fn set_error_and_set_hint_pick_the_colour() {
        let mut state = SettingsState::new(&Settings::default(), AuthState::LoggedOut);
        state.set_error("Could not save: nope");
        assert!(state.status_is_error);
        state.set_hint("Saved.");
        assert!(!state.status_is_error);
        assert_eq!(state.status_line.as_deref(), Some("Saved."));
    }

    #[test]
    fn current_binding_reads_the_right_field() {
        let mut settings = Settings::default();
        settings.bindings.mute = Some(KeyCode::named("VolumeMute"));

        assert_eq!(
            current_binding(&settings, BindingTarget::VolumeUp),
            Some(settings.bindings.volume_up.clone())
        );
        assert_eq!(
            current_binding(&settings, BindingTarget::VolumeDown),
            Some(settings.bindings.volume_down.clone())
        );
        assert_eq!(
            current_binding(&settings, BindingTarget::Mute),
            settings.bindings.mute.clone()
        );
    }
}
