//! PKCE login with a loopback redirect listener, token restore and logout.

use rspotify::AuthCodePkceSpotify;
use tokio_util::sync::CancellationToken;

use super::{SpotifyConfig, UserFacing};

/// Build an `AuthCodePkceSpotify` (client ID only, cached + auto-refreshed token).
pub fn build_client(cfg: &SpotifyConfig) -> AuthCodePkceSpotify {
    let _ = cfg;
    todo!("WP1: Credentials{{id, secret: None}}, OAuth{{redirect_uri, scopes}}, Config{{cache_path, token_cached, token_refreshing}}")
}

/// Load the cached token (refreshing it if expired). `Ok(true)` when logged in.
pub async fn restore_session(client: &AuthCodePkceSpotify) -> Result<bool, UserFacing> {
    let _ = client;
    todo!("WP1: read_token_cache(true) -> set token -> refresh if expired; guard lost refresh_token")
}

/// Bind `127.0.0.1:{port}` first, then open the browser, wait for the
/// redirect (cancellable, 5 minute timeout), exchange the code, cache the token.
pub async fn interactive_login(
    client: &mut AuthCodePkceSpotify,
    port: u16,
    cancel: CancellationToken,
    on_url: impl FnOnce(String),
) -> Result<(), UserFacing> {
    let _ = (client, port, cancel, on_url);
    todo!("WP1: PKCE login flow")
}

/// Forget the token in memory and delete the cache file.
pub async fn logout(client: &AuthCodePkceSpotify, cfg: &SpotifyConfig) {
    let _ = (client, cfg);
    todo!("WP1: clear token + remove cache file")
}
