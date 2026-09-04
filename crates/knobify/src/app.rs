//! The eframe application: owns all state, routes events, drives the OSD root
//! window and the Settings child viewport. Finalised by WP5 (integration).

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use knobify_core::config;
use knobify_core::spotify::{
    spawn_spotify, AuthState, SpotifyCmd, SpotifyConfig, SpotifyEvent, SpotifyHandle, UserFacing,
};
use knobify_core::{
    AppEvent, BindingTarget, HotkeyAction, KeyCode, Readback, Settings, TickPlan, TrayAction,
    VolumeModel,
};

use crate::handle::AppHandle;
use crate::hotkeys::{spawn_hotkey_thread, HotkeyController};
use crate::ui::osd::{self, MsgKind, OsdContent, OsdState};
use crate::ui::settings::{self, SettingsAction, SettingsState};
use crate::ui::tray::TrayUi;

/// How often a tick on a device that refuses volume control may re-read the
/// playback state to find out whether that is still true.
const DEVICE_PROBE_INTERVAL: Duration = Duration::from_secs(5);

pub struct KnobifyApp {
    settings: Settings,
    cfg_path: PathBuf,
    rx: mpsc::Receiver<AppEvent>,
    spotify: SpotifyHandle,
    hotkeys: HotkeyController,
    tray: TrayUi,
    volume: VolumeModel,
    /// Holds the first tick of a turn while the real volume is read back, so
    /// a volume changed in Spotify does not make the knob jump.
    readback: Readback,
    auth: AuthState,
    osd: OsdState,
    settings_ui: Option<Arc<Mutex<SettingsState>>>,
    suppress_unavailable: bool,
    /// False once a playback snapshot reported a device that refuses remote
    /// volume changes; the next tick then explains that instead of moving a bar
    /// that cannot move.
    device_allows_volume: bool,
    /// Last time a blocked tick asked for a fresh playback snapshot.
    last_device_probe: Option<Instant>,
    /// How many passes have re-applied the popup window's extended styles.
    /// eframe unhides the root right after its first painted frame and winit
    /// rewrites the whole `GWL_EXSTYLE` word when it does, so once is not enough.
    styled_passes: u8,
}

impl KnobifyApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        cfg_path: PathBuf,
    ) -> anyhow::Result<Self> {
        let (handle, rx) = AppHandle::new(cc.egui_ctx.clone());

        let tray = TrayUi::new(handle.clone()).context("creating the tray icon")?;
        tray.set_state(&AuthState::LoggedOut, !settings.client_id.trim().is_empty());
        let hotkeys = spawn_hotkey_thread(settings.bindings.clone(), handle.clone());

        let cache_path = config::token_cache_path().context("locating the token cache")?;
        let spotify = spawn_spotify(
            SpotifyConfig {
                client_id: settings.client_id.clone(),
                redirect_port: settings.redirect_port,
                cache_path,
            },
            Arc::new(move |event| handle.send_spotify(event)),
        );

        let mut readback = Readback::default();
        readback.set_stale_after(settings.sync_delay());

        Ok(Self {
            settings,
            cfg_path,
            rx,
            spotify,
            hotkeys,
            tray,
            volume: VolumeModel::default(),
            readback,
            auth: AuthState::LoggedOut,
            osd: OsdState::default(),
            settings_ui: None,
            suppress_unavailable: false,
            device_allows_volume: true,
            last_device_probe: None,
            styled_passes: 0,
        })
    }

    /// No Spotify client ID yet: nothing can work until the user visits Settings.
    fn needs_setup(&self) -> bool {
        self.settings.client_id.trim().is_empty()
    }

    /// While a rebind is waiting, take the first key Windows reports as pressed
    /// and make it the binding.
    ///
    /// This asks Windows directly rather than waiting for the keyboard hook, so
    /// rebinding works even when the hook is not delivering - which is worth the
    /// polling, because a rebind that silently does nothing leaves the user with
    /// no way to fix a wrong binding.
    fn poll_capture(&mut self, ctx: &egui::Context) {
        let Some(state) = self.settings_ui.clone() else {
            return;
        };
        let Ok(target) = state.lock().map(|s| s.capture) else {
            return;
        };
        let Some(target) = target else { return };

        // Keep polling: this must be the ROOT viewport, because that is the one
        // whose pass runs `App::logic`. Repainting the settings viewport instead
        // runs only its own callback, which polls nothing - so the rebind would
        // sample the keyboard once and then wait forever.
        ctx.request_repaint_of(egui::ViewportId::ROOT);

        let Some(vk) = crate::win::poll_pressed_key() else {
            return;
        };
        let key = KeyCode::from_vk(vk);
        log::info!("rebound {target:?} to {key} (vk 0x{vk:02X})");
        self.with_settings_ui(ctx, |s| {
            match target {
                BindingTarget::VolumeUp => s.draft.bindings.volume_up = key.clone(),
                BindingTarget::VolumeDown => s.draft.bindings.volume_down = key.clone(),
                BindingTarget::Mute => s.draft.bindings.mute = Some(key.clone()),
            }
            s.capture = None;
            s.status_line = Some(format!("{} set to {key}. Save to apply.", target.label()));
            s.status_is_error = false;
        });
    }

    /// Edit the settings window's shared state (if it is open) and repaint it.
    fn with_settings_ui(&self, ctx: &egui::Context, edit: impl FnOnce(&mut SettingsState)) {
        let Some(state) = &self.settings_ui else {
            return;
        };
        match state.lock() {
            Ok(mut guard) => edit(&mut guard),
            Err(_) => return,
        }
        // The settings window is a child viewport with its own repaint schedule.
        ctx.request_repaint_of(settings::viewport_id());
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

    /// Apply one knob tick to the volume model.
    fn step(&mut self, action: HotkeyAction) {
        let step = self.settings.step;
        match action {
            HotkeyAction::VolumeUp => self.volume.step_up(step),
            HotkeyAction::VolumeDown => self.volume.step_down(step),
            HotkeyAction::MuteToggle => self.volume.toggle_mute(),
        };
    }

    /// A knob tick: show it immediately, then either send it or hold it while
    /// the current volume is read back from Spotify.
    ///
    /// The tick is a relative one ("5% louder"), so it is only as right as the
    /// value it is added to. When that value may have gone stale - somebody
    /// moved the slider in the Spotify app, or another device took over - the
    /// request waits for the truth instead of jumping the volume to it. The
    /// popup does not wait, because a popup that lags behind the knob feels
    /// broken; it corrects itself if the reading disagrees.
    fn on_tick(&mut self, action: HotkeyAction, now: Instant) {
        self.step(action);
        // Logged here rather than in the hook, where writing to a file risks
        // the callback being timed out and the hook destroyed.
        log::debug!("{action:?} -> {}%", self.volume.local);
        self.show_volume(now, true);

        match self.readback.on_tick(action, now) {
            TickPlan::Send => self.spotify.set_volume(self.volume.local),
            TickPlan::ReadBack => {
                log::debug!("volume baseline is stale; reading it back before sending");
                self.spotify.send(SpotifyCmd::ReadVolume);
            }
            // The read-back already on its way will carry this tick as well.
            TickPlan::Wait => {}
        }
    }

    /// The reading the held ticks were waiting for: replay them on top of what
    /// Spotify actually reports and send that.
    fn apply_readback(&mut self, remote: u8, now: Instant) {
        let Some(actions) = self.readback.take_replay() else {
            return;
        };
        let shown = (self.volume.local, self.volume.is_muted());
        self.volume.sync_remote(remote);
        for action in actions {
            self.step(action);
        }
        if (self.volume.local, self.volume.is_muted()) != shown {
            log::debug!(
                "Spotify was at {remote}%: correcting {}% to {}%",
                shown.0,
                self.volume.local
            );
            self.show_volume(now, true);
        }
        self.spotify.set_volume(self.volume.local);
    }

    /// No reading arrived (offline, no active device, a device that refuses to
    /// be read): send what the knob asked for rather than swallow the turn.
    fn abandon_readback(&mut self) {
        if self.readback.abandon() {
            log::debug!("no volume reading; sending {}% as asked", self.volume.local);
            self.spotify.set_volume(self.volume.local);
        }
    }

    fn handle_event(&mut self, event: AppEvent, ctx: &egui::Context, now: Instant) {
        match event {
            AppEvent::Hotkey(action) => {
                if !self.device_allows_volume {
                    // Nothing to send: this device rejects remote volume changes.
                    log::debug!("ignoring {action:?}: the active device disallows volume control");
                    self.show_message(OsdContent::from(&UserFacing::VolumeControlNotAllowed), now);
                    // The user may meanwhile have switched to a device that does
                    // allow it, so re-read the playback state now and then; the
                    // snapshot clears this flag again.
                    let due = self.last_device_probe.is_none_or(|at| {
                        now.saturating_duration_since(at) >= DEVICE_PROBE_INTERVAL
                    });
                    if due {
                        self.last_device_probe = Some(now);
                        self.spotify.send(SpotifyCmd::Refresh);
                    }
                    return;
                }
                self.on_tick(action, now);
            }
            AppEvent::HookFallback(reason) => {
                log::warn!("key hook unavailable: {reason}");
                self.suppress_unavailable = true;
                self.with_settings_ui(ctx, |s| s.suppress_unavailable = true);
            }
            AppEvent::Tray(TrayAction::OpenSettings) => self.open_settings(),
            AppEvent::Tray(TrayAction::LoginOrLogout) => {
                if self.needs_setup() {
                    // The tray item reads "Set up Spotifyâ€¦" in this state.
                    self.open_settings();
                } else if self.auth.is_logged_in() {
                    self.spotify.send(SpotifyCmd::Logout);
                } else {
                    self.spotify.send(SpotifyCmd::Login);
                }
            }
            AppEvent::Tray(TrayAction::Exit) => {
                log::info!("exit requested from the tray");
                self.spotify.send(SpotifyCmd::Shutdown);
                // Closing the root viewport ends `run_native`, and returning
                // from `main` ends the process (the hook thread is never
                // joined: it blocks in `GetMessageA` forever).
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            AppEvent::Spotify(event) => self.handle_spotify(event, ctx, now),
        }
    }

    fn handle_spotify(&mut self, event: SpotifyEvent, ctx: &egui::Context, now: Instant) {
        match event {
            SpotifyEvent::Auth(auth) => {
                self.tray.set_state(&auth, !self.needs_setup());
                if !auth.is_logged_in() {
                    // Whatever the last device allowed is no longer relevant;
                    // ticks should report the login state instead.
                    self.device_allows_volume = true;
                    self.last_device_probe = None;
                }
                // A volume read from another session is not a baseline.
                self.readback.set_logged_in(auth.is_logged_in());
                let for_ui = auth.clone();
                self.with_settings_ui(ctx, move |s| s.auth = for_ui);
                // The actor refreshes the playback state itself after a restore
                // or a login; asking again here would only double every request.
                self.auth = auth;
            }
            SpotifyEvent::Playback(snapshot) => {
                if self.device_allows_volume != snapshot.supports_volume {
                    log::info!(
                        "device {:?}: volume control {}",
                        snapshot.device_name,
                        if snapshot.supports_volume {
                            "available"
                        } else {
                            "not allowed"
                        }
                    );
                }
                self.device_allows_volume = snapshot.supports_volume;

                match snapshot.volume {
                    // The reading a held tick asked for. It was requested only
                    // once nothing of ours could still be settling, so it is
                    // the truth even though the knob has just moved.
                    Some(volume) if self.readback.is_pending() => {
                        self.readback.on_synced(now);
                        self.apply_readback(volume, now);
                    }
                    // Otherwise Spotify only gets to move the model when it
                    // could not be echoing a change of ours back at us, and
                    // not while the knob is turning.
                    Some(volume) if self.readback.may_adopt(now) => {
                        if volume != self.volume.local {
                            log::debug!(
                                "adopting the device volume {volume}% over the local {}%",
                                self.volume.local
                            );
                        }
                        self.volume.sync_remote(volume);
                        self.readback.on_synced(now);
                    }
                    Some(volume) => {
                        log::debug!(
                            "ignoring the device volume {volume}%: a change of ours may still                              be settling, or the knob is moving"
                        );
                    }
                    // The device has no volume to report, so a held tick has
                    // nothing to wait for.
                    None => self.abandon_readback(),
                }
            }
            SpotifyEvent::VolumeApplied(volume) => {
                // Spotify accepted this value, so the device is at it: the
                // baseline is fresh and the rest of the turn needs no reading.
                self.readback.on_synced(now);
                // Everything the user asked for has landed: drop the
                // "pending" dot. This must not touch the burst guard - the
                // knob may well still be turning.
                if self.volume.local == volume && self.osd.is_visible(now) {
                    self.show_volume(now, false);
                }
            }
            SpotifyEvent::Error(error) => {
                log::warn!("spotify: {error}");
                // Nothing has been sent while a read-back is held, so this
                // error is the read-back failing (or Spotify being unusable);
                // either way the turn should not wait out the timeout.
                self.abandon_readback();
                self.show_message(self.error_content(&error), now);
            }
        }
    }

    /// Popup content for a Spotify error. The very first run has no client ID
    /// yet, which is not a failure but a setup step, so it gets its own
    /// info-styled message pointing at Settings.
    fn error_content(&self, error: &UserFacing) -> OsdContent {
        if self.needs_setup() && matches!(error, UserFacing::LoginFailed(_)) {
            return OsdContent::Message {
                title: "Set up Spotify".to_owned(),
                detail: "Open Settings from the tray and paste your Spotify client ID".to_owned(),
                kind: MsgKind::Info,
            };
        }
        OsdContent::from(error)
    }

    fn open_settings(&mut self) {
        if self.settings_ui.is_none() {
            let mut state = SettingsState::new(&self.settings, self.auth.clone());
            state.suppress_unavailable = self.suppress_unavailable;
            self.settings_ui = Some(Arc::new(Mutex::new(state)));
        }
    }

    fn drain_settings_actions(&mut self, ctx: &egui::Context, now: Instant) {
        let Some(state) = self.settings_ui.clone() else {
            return;
        };
        let actions: Vec<SettingsAction> = match state.lock() {
            Ok(mut s) => std::mem::take(&mut s.actions),
            Err(_) => return,
        };
        for action in actions {
            match action {
                SettingsAction::Save(new_settings) => {
                    self.apply_settings(new_settings.sanitized(), ctx);
                }
                SettingsAction::Login => self.spotify.send(SpotifyCmd::Login),
                SettingsAction::CancelLogin => self.spotify.send(SpotifyCmd::CancelLogin),
                SettingsAction::Logout => self.spotify.send(SpotifyCmd::Logout),
                SettingsAction::Capture(target) => {
                    // Drain whatever is already held down, so the click that
                    // started the rebind cannot be mistaken for the new key.
                    let _ = crate::win::poll_pressed_key();
                    log::info!("waiting for a key to bind to {target:?}");
                    self.with_settings_ui(ctx, |s| {
                        s.capture = Some(target);
                        s.status_line = Some(format!("Press the key for {}â€¦", target.label()));
                        s.status_is_error = false;
                    });
                }
                SettingsAction::CancelCapture => {
                    self.with_settings_ui(ctx, |s| {
                        s.capture = None;
                        s.status_line = None;
                    });
                }
                SettingsAction::PreviewOsd => self.show_volume(now, false),
                SettingsAction::Close => {
                    // Dropping the state ends any rebind with it: the capture
                    // must not outlive the window that started it.
                    // Not showing the deferred viewport in `App::ui` closes it.
                    self.settings_ui = None;
                }
            }
        }
    }

    /// Persist and apply already-sanitized settings.
    fn apply_settings(&mut self, new_settings: Settings, ctx: &egui::Context) {
        let client_id_changed = new_settings.client_id != self.settings.client_id;
        if client_id_changed {
            self.spotify
                .send(SpotifyCmd::SetClientId(new_settings.client_id.clone()));
        }
        if new_settings.redirect_port != self.settings.redirect_port {
            self.spotify
                .send(SpotifyCmd::SetRedirectPort(new_settings.redirect_port));
        }
        if new_settings.sync_delay_ms != self.settings.sync_delay_ms {
            self.readback.set_stale_after(new_settings.sync_delay());
        }
        if new_settings.bindings != self.settings.bindings {
            self.hotkeys.set_bindings(new_settings.bindings.clone());
        }
        if new_settings.osd != self.settings.osd {
            // Position, margin and the popup size all feed the placement.
            self.osd.needs_reposition = true;
        }
        let saved = config::save(&self.cfg_path, &new_settings);
        self.settings = new_settings;
        if client_id_changed {
            self.tray.set_state(&self.auth, !self.needs_setup());
        }

        let settings_copy = self.settings.clone();
        match saved {
            Ok(()) => {
                log::info!("settings saved to {}", self.cfg_path.display());
                self.with_settings_ui(ctx, move |s| {
                    // The file holds the sanitized values, so the window must
                    // show them too (and stop offering Save for a no-op).
                    s.draft = settings_copy.clone();
                    s.saved = settings_copy;
                    s.set_hint("Saved.");
                });
            }
            Err(e) => {
                log::error!("{e}");
                self.with_settings_ui(ctx, move |s| s.set_error(format!("Could not save: {e}")));
            }
        }
    }
}

impl eframe::App for KnobifyApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let now = Instant::now();

        while let Ok(event) = self.rx.try_recv() {
            self.handle_event(event, ctx, now);
        }
        self.drain_settings_actions(ctx, now);
        self.poll_capture(ctx);

        if self.readback.expired(now) {
            self.abandon_readback();
        }
        if let Some(deadline) = self.readback.deadline() {
            ctx.request_repaint_after(deadline.saturating_duration_since(now));
        }

        // Only ever place the window while it has something to show: while idle
        // it is parked off-screen (that is what makes it invisible), and moving
        // it onto the desktop for a settings change would leave it there.
        if self.osd.needs_reposition && self.osd.is_visible(now) {
            let pos = osd::compute_position(frame, ctx, &self.settings.osd);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
            self.osd.needs_reposition = false;
        }
        let keep_visible = self.settings_ui.is_some();
        self.osd
            .tick(ctx, frame, &self.settings.osd, keep_visible, now);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let now = Instant::now();

        // eframe keeps the root hidden until the first frame is painted and then
        // calls `set_visible(true)`, which makes winit rewrite the whole
        // `GWL_EXSTYLE` word - dropping the bits added during that same frame.
        // So apply them on the first two passes, and make sure a second pass
        // happens (an idle popup asks for no repaints at all).
        if self.styled_passes < 2 {
            self.styled_passes += 1;
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
            ui.ctx().request_repaint();
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
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.settings.osd.transparent {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            [0.11, 0.11, 0.11, 1.0]
        }
    }
}
