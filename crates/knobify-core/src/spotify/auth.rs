//! PKCE login with a loopback redirect listener, token restore and logout.

use std::io::ErrorKind;
use std::time::Duration;

use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::{scopes, AuthCodePkceSpotify, Config, Credentials, OAuth, Token};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use super::errors::classify;
use super::{SpotifyConfig, UserFacing, SCOPES};

/// How long we keep the loopback listener open waiting for Spotify.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// A browser request line is a few hundred bytes; anything longer is junk.
const MAX_REQUEST_LINE: u64 = 8 * 1024;
/// The path of our redirect URI (see [`SpotifyConfig::redirect_uri`]).
const CALLBACK_PATH: &str = "/callback";

/// Build an `AuthCodePkceSpotify` (client ID only, cached + auto-refreshed token).
pub fn build_client(cfg: &SpotifyConfig) -> AuthCodePkceSpotify {
    let creds = Credentials {
        id: cfg.client_id.trim().to_owned(),
        // PKCE never sends a client secret.
        secret: None,
    };
    let oauth = OAuth {
        redirect_uri: cfg.redirect_uri(),
        // `scopes!` splits on whitespace, so joining keeps `SCOPES` the single
        // source of truth.
        scopes: scopes!(SCOPES.join(" ")),
        ..Default::default()
    };
    let config = Config {
        cache_path: cfg.cache_path.clone(),
        token_cached: true,
        token_refreshing: true,
        ..Default::default()
    };
    AuthCodePkceSpotify::with_config(creds, oauth, config)
}

/// Replace the in-memory token. Returns an error only if the mutex is poisoned
/// (impossible with the async mutex rspotify uses for the reqwest backend).
async fn store_token(client: &AuthCodePkceSpotify, token: Option<Token>) -> Result<(), UserFacing> {
    match client.token.lock().await {
        Ok(mut guard) => {
            *guard = token;
            Ok(())
        }
        Err(_) => Err(UserFacing::Other(
            "the Spotify token lock is poisoned".to_owned(),
        )),
    }
}

/// Forget the token in memory only, keeping the cache file. Used when the
/// client ID changes: a token issued for another application is useless.
pub async fn logout_in_memory(client: &AuthCodePkceSpotify) -> Result<(), UserFacing> {
    store_token(client, None).await
}

/// True when a token is currently loaded.
pub async fn has_token(client: &AuthCodePkceSpotify) -> bool {
    match client.token.lock().await {
        Ok(guard) => guard.is_some(),
        Err(_) => false,
    }
}

/// Load the cached token (refreshing it if expired). `Ok(true)` when logged in.
pub async fn restore_session(client: &AuthCodePkceSpotify) -> Result<bool, UserFacing> {
    // `read_token_cache` fails (rather than returning `None`) when the cache
    // file is missing or unreadable, which is the normal first-run state.
    let cached = match client.read_token_cache(true).await {
        Ok(Some(token)) => token,
        Ok(None) => return Ok(false),
        Err(e) => {
            log::debug!("no usable Spotify token cache: {e}");
            return Ok(false);
        }
    };

    let expired = cached.is_expired();
    let previous_refresh = cached.refresh_token.clone();
    store_token(client, Some(cached)).await?;

    if !expired {
        return Ok(true);
    }

    if let Err(e) = client.refresh_token().await {
        // Leave the (stale) token in place; the caller reports "logged out".
        return Err(classify(e).await);
    }

    // PKCE refresh tokens rotate on every use. If the response came back
    // without one, keep the previous token so the next launch can still
    // refresh instead of forcing a browser login.
    let recovered = {
        match client.token.lock().await {
            Ok(mut guard) => match guard.as_mut() {
                Some(token) if token.refresh_token.is_none() => {
                    log::warn!("refresh response had no refresh_token; keeping the previous one");
                    token.refresh_token = previous_refresh;
                    true
                }
                Some(_) => false,
                // Refreshing cleared the token: no refresh token to begin with.
                None => return Ok(false),
            },
            Err(_) => {
                return Err(UserFacing::Other(
                    "the Spotify token lock is poisoned".to_owned(),
                ))
            }
        }
    };
    if recovered {
        if let Err(e) = client.write_token_cache().await {
            log::warn!("cannot write the Spotify token cache: {e}");
        }
    }

    Ok(true)
}

/// Bind `127.0.0.1:{port}` first, then open the browser, wait for the
/// redirect (cancellable, 5 minute timeout), exchange the code, cache the token.
pub async fn interactive_login(
    client: &mut AuthCodePkceSpotify,
    port: u16,
    cancel: CancellationToken,
    on_url: impl FnOnce(String),
) -> Result<(), UserFacing> {
    ensure_cache_dir(client);

    // Bind before opening the browser so the redirect can never race us.
    let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
        UserFacing::LoginFailed(format!(
            "port {port} is in use or blocked ({e}); pick another redirect port in Settings"
        ))
    })?;

    let url = client.get_authorize_url(None).map_err(|e| {
        UserFacing::LoginFailed(format!("cannot build the Spotify authorize URL: {e}"))
    })?;
    on_url(url.clone());
    open_browser(&url);

    let timeout = tokio::time::sleep(LOGIN_TIMEOUT);
    tokio::pin!(timeout);

    let code = loop {
        let mut stream = tokio::select! {
            () = cancel.cancelled() => return Err(UserFacing::LoginFailed("login cancelled".to_owned())),
            () = &mut timeout => {
                return Err(UserFacing::LoginFailed(
                    "Spotify did not come back within 5 minutes".to_owned(),
                ))
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => stream,
                Err(e) => {
                    log::warn!("cannot accept a redirect connection: {e}");
                    continue;
                }
            },
        };

        // Browsers also ask for /favicon.ico and friends: answer and keep
        // waiting for the real redirect.
        let Some(target) = read_request_target(&mut stream).await else {
            respond(&mut stream, "400 Bad Request", "Knobify", "Bad request.").await;
            continue;
        };
        if !target.starts_with(CALLBACK_PATH) {
            log::debug!("ignoring redirect listener request for {target}");
            respond(&mut stream, "404 Not Found", "Knobify", "Not found.").await;
            continue;
        }

        let full = format!("http://127.0.0.1:{port}{target}");
        if let Some(error) = query_param(&full, "error") {
            let message = if error == "access_denied" {
                "access denied".to_owned()
            } else {
                format!("Spotify refused the login ({error})")
            };
            respond(
                &mut stream,
                "200 OK",
                "Knobify - login failed",
                "Login failed. You can close this tab and try again from Knobify.",
            )
            .await;
            return Err(UserFacing::LoginFailed(message));
        }

        match client.parse_response_code(&full) {
            Some(code) => {
                respond(
                    &mut stream,
                    "200 OK",
                    "Knobify",
                    "You're logged in, you can close this tab",
                )
                .await;
                break code;
            }
            None => {
                respond(
                    &mut stream,
                    "200 OK",
                    "Knobify - login failed",
                    "Login failed. You can close this tab and try again from Knobify.",
                )
                .await;
                return Err(UserFacing::LoginFailed(
                    "Spotify returned no code / state mismatch".to_owned(),
                ));
            }
        }
    };

    // Caches the token as a side effect (`token_cached` is on).
    if let Err(e) = client.request_token(&code).await {
        return Err(UserFacing::LoginFailed(format!(
            "could not exchange the code for a token: {}",
            classify(e).await
        )));
    }

    Ok(())
}

/// Forget the token in memory and delete the cache file.
pub async fn logout(client: &AuthCodePkceSpotify, cfg: &SpotifyConfig) {
    if let Err(e) = store_token(client, None).await {
        log::warn!("cannot clear the in-memory Spotify token: {e}");
    }
    // `std::fs` on purpose: rspotify writes the cache synchronously too, and
    // enabling tokio's `fs` feature for two calls per session is not worth it.
    match std::fs::remove_file(&cfg.cache_path) {
        Ok(()) => log::info!("removed {}", cfg.cache_path.display()),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => log::warn!("cannot remove {}: {e}", cfg.cache_path.display()),
    }
}

/// Opening a browser is a convenience: Settings always shows the URL too, so a
/// failure is only logged. Tests never launch anything.
#[cfg(not(test))]
fn open_browser(url: &str) {
    if let Err(e) = webbrowser::open(url) {
        log::warn!("cannot open a browser ({e}); use the URL shown in Settings");
    }
}

#[cfg(test)]
fn open_browser(_url: &str) {}

/// `Token::write_cache` does not create directories, so make sure the parent
/// of the cache path exists before anything tries to write it.
fn ensure_cache_dir(client: &AuthCodePkceSpotify) {
    let Some(parent) = client.config.cache_path.parent() else {
        return;
    };
    if parent.as_os_str().is_empty() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(parent) {
        log::warn!("cannot create {}: {e}", parent.display());
    }
}

/// Read the request target ("/callback?code=..") from the first line of an
/// HTTP request, bounded so a rogue client cannot make us allocate.
async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut line = String::new();
    let mut reader = BufReader::with_capacity(1024, stream).take(MAX_REQUEST_LINE);
    match reader.read_line(&mut line).await {
        Ok(0) => return None,
        Ok(_) => {}
        Err(e) => {
            log::debug!("cannot read the redirect request: {e}");
            return None;
        }
    }

    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if !method.eq_ignore_ascii_case("GET") {
        log::debug!("ignoring a {method} request on the redirect listener");
        return None;
    }
    Some(target.to_owned())
}

fn query_param(url: &str, key: &str) -> Option<String> {
    url::Url::parse(url).ok().and_then(|url| {
        url.query_pairs()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.into_owned())
    })
}

async fn respond(stream: &mut TcpStream, status: &str, title: &str, message: &str) {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>{title}</title></head>\
         <body style=\"font:16px system-ui,sans-serif;margin:4rem;text-align:center\">\
         <p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n{body}",
        len = body.len()
    );
    if let Err(e) = stream.write_all(response.as_bytes()).await {
        log::debug!("cannot write the redirect response: {e}");
    }
    if let Err(e) = stream.shutdown().await {
        log::debug!("cannot close the redirect connection: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg(dir: &std::path::Path) -> SpotifyConfig {
        SpotifyConfig {
            client_id: "  abc123  ".to_owned(),
            redirect_port: 8888,
            cache_path: dir.join("knobify").join("token.json"),
        }
    }

    #[test]
    fn client_is_built_for_pkce() {
        let client = build_client(&cfg(&PathBuf::from("/tmp")));
        assert_eq!(client.creds.id, "abc123");
        assert_eq!(client.creds.secret, None);
        assert_eq!(client.oauth.redirect_uri, "http://127.0.0.1:8888/callback");
        assert_eq!(client.oauth.scopes.len(), SCOPES.len());
        for scope in SCOPES {
            assert!(client.oauth.scopes.contains(scope), "missing {scope}");
        }
        assert!(client.config.token_cached);
        assert!(client.config.token_refreshing);
        assert_eq!(
            client.config.cache_path,
            PathBuf::from("/tmp/knobify/token.json")
        );
    }

    #[tokio::test]
    async fn restoring_without_a_cache_file_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let client = build_client(&cfg(dir.path()));
        assert_eq!(restore_session(&client).await, Ok(false));
        assert!(!has_token(&client).await);
    }

    /// Writes a token cache file by hand: `Token` carries `chrono` types that
    /// this crate does not depend on directly, but its serde form is stable.
    fn write_token_cache(cfg: &SpotifyConfig, scopes: &str) {
        let parent = cfg.cache_path.parent().expect("cache path has a parent");
        std::fs::create_dir_all(parent).expect("mkdir");
        let json = format!(
            r#"{{"access_token":"at","expires_in":3600,"expires_at":"2999-01-01T00:00:00Z","refresh_token":"rt","scope":"{scopes}"}}"#
        );
        std::fs::write(&cfg.cache_path, json).expect("write cache");
    }

    #[tokio::test]
    async fn restoring_a_valid_cached_token_logs_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = cfg(dir.path());
        write_token_cache(&cfg, &SCOPES.join(" "));

        let client = build_client(&cfg);
        assert_eq!(restore_session(&client).await, Ok(true));
        assert!(has_token(&client).await);

        logout(&client, &cfg).await;
        assert!(!has_token(&client).await);
        assert!(!cfg.cache_path.exists());
        // Logging out twice must not complain about the missing file.
        logout(&client, &cfg).await;
    }

    #[tokio::test]
    async fn a_cached_token_with_missing_scopes_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = cfg(dir.path());
        write_token_cache(&cfg, SCOPES[0]);

        let client = build_client(&cfg);
        assert_eq!(restore_session(&client).await, Ok(false));
    }

    #[tokio::test]
    async fn login_reports_a_busy_port_without_opening_a_browser() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocker = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = blocker.local_addr().expect("addr").port();

        let mut cfg = cfg(dir.path());
        cfg.redirect_port = port;
        let mut client = build_client(&cfg);

        let mut opened = None;
        let result = interactive_login(&mut client, port, CancellationToken::new(), |url| {
            opened = Some(url);
        })
        .await;

        assert!(opened.is_none(), "must fail before showing a URL");
        match result {
            Err(UserFacing::LoginFailed(msg)) => assert!(
                msg.contains(&port.to_string()) && msg.contains("in use"),
                "unhelpful message: {msg}"
            ),
            other => panic!("expected LoginFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn login_can_be_cancelled_while_waiting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cfg = cfg(dir.path());
        // Port 0 lets the OS pick a free one; the URL will be wrong but we
        // never follow it here.
        cfg.redirect_port = 0;
        let mut client = build_client(&cfg);

        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            trigger.cancel();
        });

        let mut url = None;
        let result = interactive_login(&mut client, 0, cancel, |u| url = Some(u)).await;

        let url = url.expect("the authorize URL must be published before waiting");
        assert!(
            url.starts_with("https://accounts.spotify.com/authorize?"),
            "{url}"
        );
        assert!(url.contains("code_challenge_method=S256"), "{url}");
        assert_eq!(
            result,
            Err(UserFacing::LoginFailed("login cancelled".to_owned()))
        );
        // The cache directory is created eagerly so `request_token` can write.
        assert!(dir.path().join("knobify").is_dir());
    }

    #[tokio::test]
    async fn the_listener_ignores_requests_that_are_not_the_callback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let probe = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = probe.local_addr().expect("addr").port();
        drop(probe);

        let mut cfg = cfg(dir.path());
        cfg.redirect_port = port;
        let mut client = build_client(&cfg);

        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            // Give `interactive_login` a moment to bind, then knock twice.
            for _ in 0..50 {
                if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await {
                    let _ = stream
                        .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n")
                        .await;
                    let mut sink = Vec::new();
                    let _ = stream.read_to_end(&mut sink).await;
                    assert!(
                        String::from_utf8_lossy(&sink).starts_with("HTTP/1.1 404"),
                        "favicon should get a 404"
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            trigger.cancel();
        });

        let result = interactive_login(&mut client, port, cancel, |_| {}).await;
        assert_eq!(
            result,
            Err(UserFacing::LoginFailed("login cancelled".to_owned())),
            "a favicon request must not end the login"
        );
    }

    #[tokio::test]
    async fn access_denied_is_reported_as_such() {
        let dir = tempfile::tempdir().expect("tempdir");
        let probe = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = probe.local_addr().expect("addr").port();
        drop(probe);

        let mut cfg = cfg(dir.path());
        cfg.redirect_port = port;
        let mut client = build_client(&cfg);
        let state = client.oauth.state.clone();

        tokio::spawn(async move {
            for _ in 0..50 {
                if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await {
                    let request = format!(
                        "GET /callback?error=access_denied&state={state} HTTP/1.1\r\nHost: x\r\n\r\n"
                    );
                    let _ = stream.write_all(request.as_bytes()).await;
                    let mut sink = Vec::new();
                    let _ = stream.read_to_end(&mut sink).await;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });

        let result = interactive_login(&mut client, port, CancellationToken::new(), |_| {}).await;
        assert_eq!(
            result,
            Err(UserFacing::LoginFailed("access denied".to_owned()))
        );
    }

    #[tokio::test]
    async fn a_state_mismatch_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let probe = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = probe.local_addr().expect("addr").port();
        drop(probe);

        let mut cfg = cfg(dir.path());
        cfg.redirect_port = port;
        let mut client = build_client(&cfg);

        tokio::spawn(async move {
            for _ in 0..50 {
                if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await {
                    let _ = stream
                        .write_all(
                            b"GET /callback?code=abc&state=not-ours HTTP/1.1\r\nHost: x\r\n\r\n",
                        )
                        .await;
                    let mut sink = Vec::new();
                    let _ = stream.read_to_end(&mut sink).await;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });

        let result = interactive_login(&mut client, port, CancellationToken::new(), |_| {}).await;
        match result {
            Err(UserFacing::LoginFailed(msg)) => {
                assert!(msg.contains("state mismatch"), "{msg}");
            }
            other => panic!("expected LoginFailed, got {other:?}"),
        }
    }
}
