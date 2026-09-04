//! The Spotify actor loop: owns the client, coalesces volume changes,
//! reconciles with `current_playback`, and reports state to the UI.

use tokio::sync::mpsc;

use super::{AuthState, EventSink, SpotifyCmd, SpotifyConfig, SpotifyEvent, UserFacing};

/// Runs until `Shutdown` is received or the command channel closes.
///
/// Skeleton behaviour (replaced by WP1): reports logged out and answers every
/// volume command with `NotLoggedIn`.
pub async fn run(cfg: SpotifyConfig, mut rx: mpsc::UnboundedReceiver<SpotifyCmd>, sink: EventSink) {
    log::info!("spotify actor started (redirect {})", cfg.redirect_uri());
    sink(SpotifyEvent::Auth(AuthState::LoggedOut));
    while let Some(cmd) = rx.recv().await {
        match cmd {
            SpotifyCmd::Shutdown => break,
            SpotifyCmd::SetVolume(_) | SpotifyCmd::Refresh => {
                sink(SpotifyEvent::Error(UserFacing::NotLoggedIn));
            }
            other => log::debug!("spotify actor skeleton ignoring {other:?}"),
        }
    }
    log::info!("spotify actor stopped");
}
