//! Spotify service: an actor on its own tokio runtime thread that owns the
//! `AuthCodePkceSpotify` client. The UI talks to it through [`SpotifyHandle`]
//! and receives [`SpotifyEvent`]s through the sink passed to [`spawn_spotify`].

pub mod actor;
pub mod auth;
pub mod errors;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

pub use errors::UserFacing;

/// The only scopes Knobify needs.
pub const SCOPES: [&str; 2] = ["user-modify-playback-state", "user-read-playback-state"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyConfig {
    pub client_id: String,
    pub redirect_port: u16,
    /// Where the OAuth token is cached (`<config dir>/knobify/token.json`).
    pub cache_path: PathBuf,
}

impl SpotifyConfig {
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.redirect_port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpotifyCmd {
    /// Desired volume (0..=100). May arrive many times per second; coalesced.
    SetVolume(u8),
    /// Re-read the current playback state (volume, device).
    Refresh,
    Login,
    CancelLogin,
    Logout,
    SetClientId(String),
    SetRedirectPort(u16),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpotifyEvent {
    Auth(AuthState),
    Playback(PlaybackSnapshot),
    /// Spotify accepted this volume.
    VolumeApplied(u8),
    Error(UserFacing),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthState {
    LoggedOut,
    /// Browser opened; the URL is also shown in Settings for copy/paste.
    LoggingIn {
        authorize_url: String,
    },
    LoggedIn {
        display_name: Option<String>,
    },
    Failed(String),
}

impl AuthState {
    pub fn is_logged_in(&self) -> bool {
        matches!(self, AuthState::LoggedIn { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackSnapshot {
    pub volume: Option<u8>,
    pub device_name: String,
    pub device_id: Option<String>,
    pub supports_volume: bool,
    pub is_playing: bool,
}

/// Receives events from the actor thread. Must be cheap and non-blocking
/// (the UI implementation pushes to a channel and requests a repaint).
pub type EventSink = Arc<dyn Fn(SpotifyEvent) + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub struct SpotifyHandle(mpsc::UnboundedSender<SpotifyCmd>);

impl SpotifyHandle {
    pub fn send(&self, cmd: SpotifyCmd) {
        if self.0.send(cmd).is_err() {
            log::warn!("spotify actor is gone; command dropped");
        }
    }

    pub fn set_volume(&self, volume: u8) {
        self.send(SpotifyCmd::SetVolume(volume.min(100)));
    }
}

/// Default backoff used when Spotify does not tell us how long to wait.
pub const DEFAULT_BACKOFF: Duration = Duration::from_secs(2);

/// Start the Spotify actor on its own thread with a current-thread tokio
/// runtime. Returns immediately.
pub fn spawn_spotify(cfg: SpotifyConfig, sink: EventSink) -> SpotifyHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let thread_sink = Arc::clone(&sink);
    let spawned = std::thread::Builder::new()
        .name("knobify-spotify".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    log::error!("cannot start tokio runtime: {e}");
                    thread_sink(SpotifyEvent::Error(UserFacing::Other(format!(
                        "cannot start Spotify service: {e}"
                    ))));
                    return;
                }
            };
            rt.block_on(actor::run(cfg, rx, thread_sink));
        });
    if let Err(e) = spawned {
        // The receiver went with the closure, so every command from the
        // returned handle will simply be logged and dropped.
        log::error!("cannot start the Spotify thread: {e}");
        sink(SpotifyEvent::Error(UserFacing::Other(format!(
            "cannot start Spotify service: {e}"
        ))));
    }
    SpotifyHandle(tx)
}
