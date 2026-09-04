//! The eframe application: owns all state, routes events, drives the OSD root
//! window and the Settings child viewport. Finalised by WP5 (integration).

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use knobify_core::config;
use knobify_core::spotify::{
    spawn_spotify, AuthState, SpotifyCmd, SpotifyConfig, SpotifyEvent, SpotifyHandle,
};
use knobify_core::{AppEvent, HotkeyAction, Settings, TrayAction, VolumeModel};

use crate::handle::AppHandle;
use crate::hotkeys::{spawn_hotkey_thread, HotkeyController};
use crate::ui::osd::{self, OsdContent, OsdState};
use crate::ui::settings::{self, SettingsAction, SettingsState};
use crate::ui::tray::TrayUi;

pub struct KnobifyApp {
    settings: Settings,
    cfg_path: PathBuf,
    rx: mpsc::Receiver<AppEvent>,
    handle: AppHandle,
    spotify: SpotifyHandle,
    hotkeys: HotkeyController,
    tray: TrayUi,
    volume: VolumeModel,
    auth: AuthState,
    osd: OsdState,
    settings_ui: Option<Arc<Mutex<SettingsState>>>,
    suppress_unavailable: bool,
    first_frame_done: bool,
}

impl KnobifyApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        cfg_path: PathBuf,
    ) -> anyhow::Result<Self> {
        let (handle, rx) = AppHandle::new(cc.egui_ctx.clone());

        let tray = TrayUi::new(handle.clone()).context("creating the tray icon")?;
        let hotkeys = spawn_hotkey_thread(settings.bindings.clone(), handle.clone());

        let cache_path = config::token_cache_path().context("locating the token cache")?;
        let sink_handle = handle.clone();
        let spotify = spawn_spotify(
            SpotifyConfig {
                client_id: settings.client_id.clone(),
                redirect_port: settings.redirect_port,
                cache_path,
            },
            Arc::new(move |event| sink_handle.send_spotify(event)),
        );

        Ok(Self {
            settings,
            cfg_path,
            rx,
            handle,
            spotify,
            hotkeys,
            tray,
            volume: VolumeModel::default(),
            auth: AuthState::LoggedOut,
            osd: OsdState::default(),
            settings_ui: None,
            suppress_unavailable: false,
            first_frame_done: false,
        })
    }

    fn osd_duration(&self) -> Duration {
        Duration::from_millis(self.settings.osd.duration_ms)
    }

    fn show_volume(&mut self, now: Instant, pending: bool) {
        if !self.settings.osd.enabled {
            return;
        }
        let content = OsdContent::Volume {
            percent: self.volume.local,
            muted: self.volume.is_muted(),
            pending,
        };
        let duration = self.osd_duration();
        self.osd.show(content, duration, now);
    }

    fn show_message(&mut self, content: OsdContent, now: Instant) {
        if !self.settings.osd.enabled {
            return;
        }
        // Messages stay a little longer than a volume flash.
        let duration = self.osd_duration().max(Duration::from_millis(2500));
        self.osd.show(content, duration, now);
    }

    fn handle_event(&mut self, event: AppEvent, ctx: &egui::Context, now: Instant) {
        match event {
            AppEvent::Hotkey(action) => {
                let step = self.settings.step;
                let new_volume = match action {
                    HotkeyAction::VolumeUp => self.volume.step_up(step),
                    HotkeyAction::VolumeDown => self.volume.step_down(step),
                    HotkeyAction::MuteToggle => self.volume.toggle_mute(),
                };
                self.spotify.set_volume(new_volume);
                self.show_volume(now, true);
            }
            AppEvent::KeyCaptured { target, key } => {
                self.hotkeys.cancel_capture();
                if let Some(state) = &self.settings_ui {
                    if let Ok(mut s) = state.lock() {
                        match target {
                            knobify_core::BindingTarget::VolumeUp => s.draft.bindings.volume_up = key,
                            knobify_core::BindingTarget::VolumeDown => {
                                s.draft.bindings.volume_down = key
                            }
                            knobify_core::BindingTarget::Mute => s.draft.bindings.mute = Some(key),
                        }
                        s.capture = None;
                    }
                }
            }
            AppEvent::CaptureCancelled => {
                self.hotkeys.cancel_capture();
                if let Some(state) = &self.settings_ui {
                    if let Ok(mut s) = state.lock() {
                        s.capture = None;
                    }
                }
            }
            AppEvent::HookFallback(reason) => {
                log::warn!("key hook fallback: {reason}");
                self.suppress_unavailable = true;
                if let Some(state) = &self.settings_ui {
                    if let Ok(mut s) = state.lock() {
                        s.suppress_unavailable = true;
                    }
                }
            }
            AppEvent::Tray(TrayAction::OpenSettings) => self.open_settings(),
            AppEvent::Tray(TrayAction::LoginOrLogout) => {
                if self.auth.is_logged_in() {
                    self.spotify.send(SpotifyCmd::Logout);
                } else {
                    self.spotify.send(SpotifyCmd::Login);
                }
            }
            AppEvent::Tray(TrayAction::Exit) => {
                self.spotify.send(SpotifyCmd::Shutdown);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            AppEvent::Spotify(event) => self.handle_spotify(event, now),
        }
    }

    fn handle_spotify(&mut self, event: SpotifyEvent, now: Instant) {
        match event {
            SpotifyEvent::Auth(auth) => {
                self.tray.set_auth(&auth);
                if let Some(state) = &self.settings_ui {
                    if let Ok(mut s) = state.lock() {
                        s.auth = auth.clone();
                    }
                }
                if auth.is_logged_in() {
                    self.spotify.send(SpotifyCmd::Refresh);
                }
                self.auth = auth;
            }
            SpotifyEvent::Playback(snapshot) => {
                if let Some(volume) = snapshot.volume {
                    self.volume.sync_remote(volume);
                }
            }
            SpotifyEvent::VolumeApplied(volume) => {
                if self.osd.is_visible(now) && self.volume.local == volume {
                    self.show_volume(now, false);
                }
            }
            SpotifyEvent::Error(error) => {
                log::warn!("spotify: {error:?}");
                self.show_message(OsdContent::from(&error), now);
            }
        }
    }

    fn open_settings(&mut self) {
        if self.settings_ui.is_none() {
            let mut state = SettingsState::new(&self.settings, self.auth.clone());
            state.suppress_unavailable = self.suppress_unavailable;
            self.settings_ui = Some(Arc::new(Mutex::new(state)));
        }
    }

    fn drain_settings_actions(&mut self, now: Instant) {
        let Some(state) = self.settings_ui.clone() else { return };
        let actions: Vec<SettingsAction> = match state.lock() {
            Ok(mut s) => std::mem::take(&mut s.actions),
            Err(_) => return,
        };
        for action in actions {
            match action {
                SettingsAction::Save(new_settings) => self.apply_settings(new_settings.sanitized()),
                SettingsAction::Login => self.spotify.send(SpotifyCmd::Login),
                SettingsAction::CancelLogin => self.spotify.send(SpotifyCmd::CancelLogin),
                SettingsAction::Logout => self.spotify.send(SpotifyCmd::Logout),
                SettingsAction::Capture(target) => {
                    self.hotkeys.begin_capture(target);
                    if let Ok(mut s) = state.lock() {
                        s.capture = Some(target);
                    }
                }
                SettingsAction::CancelCapture => {
                    self.hotkeys.cancel_capture();
                    if let Ok(mut s) = state.lock() {
                        s.capture = None;
                    }
                }
                SettingsAction::PreviewOsd => self.show_volume(now, false),
                SettingsAction::Close => {
                    self.hotkeys.cancel_capture();
                    self.settings_ui = None;
                }
            }
        }
    }

    fn apply_settings(&mut self, new_settings: Settings) {
        if new_settings.client_id != self.settings.client_id {
            self.spotify
                .send(SpotifyCmd::SetClientId(new_settings.client_id.clone()));
        }
        if new_settings.redirect_port != self.settings.redirect_port {
            self.spotify
                .send(SpotifyCmd::SetRedirectPort(new_settings.redirect_port));
        }
        if new_settings.bindings != self.settings.bindings {
            self.hotkeys.set_bindings(new_settings.bindings.clone());
        }
        if new_settings.osd != self.settings.osd {
            self.osd.needs_reposition = true;
        }
        match config::save(&self.cfg_path, &new_settings) {
            Ok(()) => log::info!("settings saved to {}", self.cfg_path.display()),
            Err(e) => {
                log::error!("{e}");
                if let Some(state) = &self.settings_ui {
                    if let Ok(mut s) = state.lock() {
                        s.status_line = Some(format!("Could not save: {e}"));
                    }
                }
            }
        }
        self.settings = new_settings;
    }
}

impl eframe::App for KnobifyApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let now = Instant::now();

        while let Ok(event) = self.rx.try_recv() {
            self.handle_event(event, ctx, now);
        }
        self.drain_settings_actions(now);

        if self.osd.needs_reposition {
            if let Some(pos) = osd::compute_position(frame, ctx, &self.settings.osd) {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
                self.osd.needs_reposition = false;
            }
        }
        self.osd.tick(ctx, now);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let now = Instant::now();

        if !self.first_frame_done {
            self.first_frame_done = true;
            if let Some(window) = frame.winit_window() {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(handle) = window.window_handle() {
                    if let RawWindowHandle::Win32(h) = handle.as_raw() {
                        crate::win::apply_osd_exstyles(h.hwnd.get());
                        if !self.settings.osd.transparent {
                            crate::win::set_rounded_corners(h.hwnd.get());
                        }
                    }
                }
            }
        }

        osd::draw(ui, &self.osd, &self.settings.osd, now);

        if let Some(state) = self.settings_ui.clone() {
            ui.ctx().show_viewport_deferred(
                settings::viewport_id(),
                settings::viewport_builder(),
                move |ctx, _class| {
                    egui::CentralPanel::default().show(ctx, |ui| settings::show(ui, &state));
                },
            );
        }

        let _ = &self.handle;
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.settings.osd.transparent {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            [0.11, 0.11, 0.11, 1.0]
        }
    }
}
