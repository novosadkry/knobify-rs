//! System tray icon and menu.
//!
//! Must be created on the UI thread after the winit event loop exists (i.e.
//! inside the eframe app constructor). Menu/tray events are forwarded through
//! `AppHandle` as `AppEvent::Tray(..)`.
//!
//! `tray_icon::menu::MenuEvent` and `tray_icon::TrayIconEvent` each hold a
//! single process-global handler (`set_event_handler`), so `TrayUi::new` must
//! only ever be called once per process.

use anyhow::Context as _;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

use knobify_core::spotify::AuthState;
use knobify_core::{AppEvent, TrayAction};

use crate::handle::AppHandle;

const ID_SETTINGS: &str = "settings";
const ID_LOGIN: &str = "login";
const ID_EXIT: &str = "exit";

const TOOLTIP_LOGGED_OUT: &str = "Knobify — not logged in";
const TOOLTIP_LOGGED_IN: &str = "Knobify — logged in";

const LABEL_LOG_IN: &str = "Log in to Spotify";
const LABEL_LOGGING_IN: &str = "Logging in…";
const LABEL_LOG_OUT: &str = "Log out";

pub struct TrayUi {
    icon: TrayIcon,
    login_item: MenuItem,
}

impl TrayUi {
    pub fn new(handle: AppHandle) -> anyhow::Result<Self> {
        let settings_item = MenuItem::with_id(ID_SETTINGS, "Settings…", true, None);
        let login_item = MenuItem::with_id(ID_LOGIN, LABEL_LOG_IN, true, None);
        let exit_item = MenuItem::with_id(ID_EXIT, "Exit", true, None);

        let menu = Menu::new();
        menu.append(&settings_item)
            .context("adding the Settings menu item")?;
        menu.append(&login_item)
            .context("adding the Login/Logout menu item")?;
        menu.append(&PredefinedMenuItem::separator())
            .context("adding the menu separator")?;
        menu.append(&exit_item)
            .context("adding the Exit menu item")?;

        let icon = crate::icon::tray_icon().context("loading the tray icon")?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_tooltip(TOOLTIP_LOGGED_OUT)
            .with_menu_on_left_click(false)
            .build()
            .context("creating the tray icon")?;

        // Both handlers below are process-global statics (see module docs):
        // set them once, here, for the lifetime of the process.
        let menu_handle = handle.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = match event.id().as_ref() {
                ID_SETTINGS => Some(TrayAction::OpenSettings),
                ID_LOGIN => Some(TrayAction::LoginOrLogout),
                ID_EXIT => Some(TrayAction::Exit),
                _ => None,
            };
            if let Some(action) = action {
                menu_handle.send(AppEvent::Tray(action));
            }
        }));

        let tray_handle = handle;
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                tray_handle.send(AppEvent::Tray(TrayAction::OpenSettings));
            }
        }));

        Ok(Self {
            icon: tray,
            login_item,
        })
    }

    /// Update the login/logout label and tooltip.
    pub fn set_auth(&self, auth: &AuthState) {
        let (label, enabled) = match auth {
            AuthState::LoggedOut | AuthState::Failed(_) => (LABEL_LOG_IN, true),
            AuthState::LoggingIn { .. } => (LABEL_LOGGING_IN, false),
            AuthState::LoggedIn { .. } => (LABEL_LOG_OUT, true),
        };
        self.login_item.set_text(label);
        self.login_item.set_enabled(enabled);

        let tooltip = if auth.is_logged_in() {
            TOOLTIP_LOGGED_IN
        } else {
            TOOLTIP_LOGGED_OUT
        };
        let _ = self.icon.set_tooltip(Some(tooltip));
    }
}
