//! The Spotify actor loop: owns the client, coalesces volume changes,
//! reconciles with `current_playback`, and reports state to the UI.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::model::{AdditionalType, CurrentPlaybackContext};
use rspotify::AuthCodePkceSpotify;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::coalescer::Coalescer;

use super::errors::classify;
use super::{
    auth, AuthState, EventSink, PlaybackSnapshot, SpotifyCmd, SpotifyConfig, SpotifyEvent,
    UserFacing, DEFAULT_BACKOFF,
};

/// The same error is never shown to the user more than once per this window.
const ERROR_THROTTLE: Duration = Duration::from_secs(2);
/// After a burst, re-read the real device volume this long after the last PUT.
const RECONCILE_AFTER: Duration = Duration::from_millis(1500);
/// A burst that starts after this much silence gets a background reconcile.
const IDLE_RECONCILE: Duration = Duration::from_secs(30);
/// After this many consecutive failed PUTs, drop the pending value.
const MAX_CONSECUTIVE_FAILURES: u32 = 2;

/// Messages that background tasks (login, background refresh) send back to the
/// actor so that all mutable state stays on the actor's own task.
enum Internal {
    LoginDone(Result<(), UserFacing>),
    /// A volume read from the device by a background refresh.
    Observed(u8),
}

/// Runs until `Shutdown` is received or the command channel closes.
pub async fn run(cfg: SpotifyConfig, mut rx: mpsc::UnboundedReceiver<SpotifyCmd>, sink: EventSink) {
    log::info!("spotify actor started (redirect {})", cfg.redirect_uri());

    let (itx, mut irx) = mpsc::unbounded_channel();
    let mut state = ActorState::new(cfg, sink);
    state.bootstrap().await;

    loop {
        let wakeup = state.next_wakeup();
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None | Some(SpotifyCmd::Shutdown) => break,
                Some(cmd) => state.handle_cmd(cmd, &itx).await,
            },
            Some(msg) = irx.recv() => state.handle_internal(msg).await,
            () = wait_until(wakeup) => state.on_timer().await,
        }
    }

    state.cancel_login();
    log::info!("spotify actor stopped");
}

/// Sleep until `deadline`, or forever when there is nothing scheduled.
async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => std::future::pending::<()>().await,
    }
}

struct ActorState {
    cfg: SpotifyConfig,
    client: AuthCodePkceSpotify,
    sink: EventSink,
    coalescer: Coalescer,
    logged_in: bool,
    login_cancel: Option<CancellationToken>,
    /// Set while a cancelled login task is still winding down.
    login_cancelled: bool,
    /// Single timer for the post-burst reconcile, reset on every send.
    reconcile_at: Option<Instant>,
    /// Last time we knew the device volume for sure (PUT accepted or read).
    last_synced: Option<Instant>,
    consecutive_failures: u32,
    /// A 401 buys one token refresh + retry per burst.
    auth_retry_used: bool,
    last_error: Option<(UserFacing, Instant)>,
    /// Set by the first volume command or `ReadVolume`. The UI also sends
    /// `Refresh` right after a login, and nobody wants "no active device" on
    /// the screen before they have even touched the knob.
    user_interacted: bool,
}

impl ActorState {
    fn new(cfg: SpotifyConfig, sink: EventSink) -> Self {
        let client = auth::build_client(&cfg);
        Self {
            cfg,
            client,
            sink,
            coalescer: Coalescer::default(),
            logged_in: false,
            login_cancel: None,
            login_cancelled: false,
            reconcile_at: None,
            last_synced: None,
            consecutive_failures: 0,
            auth_retry_used: false,
            last_error: None,
            user_interacted: false,
        }
    }

    fn emit(&self, event: SpotifyEvent) {
        (self.sink)(event);
    }

    /// Emit an error, but never the same one twice within [`ERROR_THROTTLE`],
    /// so a burst of failing volume ticks cannot flood the OSD.
    fn emit_error(&mut self, error: UserFacing) {
        let now = Instant::now();
        if let Some((previous, at)) = &self.last_error {
            if *previous == error && now.duration_since(*at) < ERROR_THROTTLE {
                log::debug!("suppressing repeated spotify error: {error}");
                return;
            }
        }
        self.last_error = Some((error.clone(), now));
        self.emit(SpotifyEvent::Error(error));
    }

    fn configured(&self) -> bool {
        !self.cfg.client_id.trim().is_empty()
    }

    /// Restore the cached session, report it and read the current playback.
    async fn bootstrap(&mut self) {
        if !self.configured() {
            log::info!("no Spotify client ID configured");
            self.set_logged_out();
            return;
        }
        match auth::restore_session(&self.client).await {
            Ok(true) => {
                self.logged_in = true;
                self.auth_retry_used = false;
                // `me()` would need the `user-read-private` scope, which
                // Knobify deliberately does not request.
                self.emit(SpotifyEvent::Auth(AuthState::LoggedIn {
                    display_name: None,
                }));
                self.refresh(false).await;
            }
            Ok(false) => self.set_logged_out(),
            Err(e) => {
                // Startup should not pop an error toast; the tray shows the
                // logged-out state and Settings offers Login.
                log::warn!("cannot restore the Spotify session: {e}");
                self.set_logged_out();
            }
        }
    }

    fn set_logged_out(&mut self) {
        self.logged_in = false;
        self.emit(SpotifyEvent::Auth(AuthState::LoggedOut));
    }

    fn next_wakeup(&self) -> Option<Instant> {
        match (self.coalescer.next_deadline(), self.reconcile_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    async fn handle_cmd(&mut self, cmd: SpotifyCmd, itx: &mpsc::UnboundedSender<Internal>) {
        match cmd {
            SpotifyCmd::SetVolume(volume) => self.on_local_volume(volume.min(100), itx),
            SpotifyCmd::Refresh => self.refresh(true).await,
            SpotifyCmd::ReadVolume => {
                // The knob has been touched even though no volume has been
                // sent yet, so "no active device" is now worth reporting.
                self.user_interacted = true;
                self.refresh(true).await;
            }
            SpotifyCmd::Login => self.start_login(itx),
            SpotifyCmd::CancelLogin => {
                if self.cancel_login() {
                    self.set_logged_out();
                }
            }
            SpotifyCmd::Logout => {
                self.cancel_login();
                auth::logout(&self.client, &self.cfg).await;
                self.coalescer = Coalescer::default();
                self.reconcile_at = None;
                self.set_logged_out();
            }
            SpotifyCmd::SetClientId(id) => {
                if id.trim() == self.cfg.client_id.trim() {
                    return;
                }
                log::info!("Spotify client ID changed");
                self.cfg.client_id = id;
                // A token issued for another client ID cannot be refreshed, so
                // drop it from memory. The cache file is left alone (in case
                // the ID is changed back) but deliberately not read again:
                // claiming to be logged in with another app's token would be
                // a lie that only surfaces an hour later.
                if let Err(e) = auth::logout_in_memory(&self.client).await {
                    log::warn!("cannot clear the in-memory Spotify token: {e}");
                }
                self.rebuild(false).await;
            }
            SpotifyCmd::SetRedirectPort(port) => {
                if port == self.cfg.redirect_port {
                    return;
                }
                log::info!("redirect port changed to {port}");
                self.cfg.redirect_port = port;
                // Only the redirect URI changed; the token stays valid.
                self.rebuild(true).await;
            }
            // Handled by `run` so the loop can exit.
            SpotifyCmd::Shutdown => {}
        }
    }

    /// Rebuild the client for a changed client ID or redirect port and report
    /// the resulting auth state.
    async fn rebuild(&mut self, restore: bool) {
        self.cancel_login();
        self.client = auth::build_client(&self.cfg);
        self.coalescer = Coalescer::default();
        self.reconcile_at = None;
        self.consecutive_failures = 0;
        self.last_error = None;
        if restore {
            self.bootstrap().await;
        } else {
            self.set_logged_out();
        }
    }

    fn on_local_volume(&mut self, volume: u8, itx: &mpsc::UnboundedSender<Internal>) {
        let now = Instant::now();
        if !self.configured() {
            self.emit_error(UserFacing::LoginFailed(
                "Set your Spotify client ID in Settings".to_owned(),
            ));
            return;
        }
        self.user_interacted = true;
        // A burst that starts after a long silence may be working from a stale
        // baseline: reconcile in the background without holding up the tick.
        if self.coalescer.is_idle() && self.stale(now) {
            self.spawn_background_refresh(itx);
        }
        self.coalescer.on_local(volume, now);
    }

    fn stale(&self, now: Instant) -> bool {
        self.logged_in
            && self
                .last_synced
                .is_none_or(|at| now.duration_since(at) > IDLE_RECONCILE)
    }

    /// Cancels a login in progress. Returns true when there was one.
    fn cancel_login(&mut self) -> bool {
        match self.login_cancel.take() {
            Some(cancel) => {
                log::info!("cancelling the Spotify login");
                cancel.cancel();
                self.login_cancelled = true;
                true
            }
            None => false,
        }
    }

    fn start_login(&mut self, itx: &mpsc::UnboundedSender<Internal>) {
        if !self.configured() {
            self.emit_error(UserFacing::LoginFailed(
                "Set your Spotify client ID in Settings".to_owned(),
            ));
            self.emit(SpotifyEvent::Auth(AuthState::Failed(
                "no client ID configured".to_owned(),
            )));
            return;
        }
        if self.login_cancel.is_some() {
            log::debug!("a Spotify login is already in progress");
            return;
        }

        // Cloning shares the token `Arc`, so the token the task obtains lands
        // in this client too; the PKCE verifier stays local to the clone that
        // built the authorize URL.
        let mut client = self.client.clone();
        let port = self.cfg.redirect_port;
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let sink = Arc::clone(&self.sink);
        let itx = itx.clone();

        tokio::spawn(async move {
            let result = auth::interactive_login(&mut client, port, token, |url| {
                sink(SpotifyEvent::Auth(AuthState::LoggingIn {
                    authorize_url: url,
                }));
            })
            .await;
            let _ = itx.send(Internal::LoginDone(result));
        });

        self.login_cancel = Some(cancel);
        self.login_cancelled = false;
    }

    async fn handle_internal(&mut self, msg: Internal) {
        match msg {
            Internal::LoginDone(result) => {
                self.login_cancel = None;
                if std::mem::take(&mut self.login_cancelled) {
                    // `CancelLogin` already reported the logged-out state.
                    log::debug!("cancelled login finished with {result:?}");
                    return;
                }
                match result {
                    Ok(()) => {
                        self.logged_in = true;
                        self.auth_retry_used = false;
                        self.last_error = None;
                        self.emit(SpotifyEvent::Auth(AuthState::LoggedIn {
                            display_name: None,
                        }));
                        self.refresh(false).await;
                    }
                    Err(e) => {
                        self.logged_in = false;
                        self.emit(SpotifyEvent::Auth(AuthState::Failed(e.to_string())));
                        self.emit_error(match e {
                            UserFacing::LoginFailed(_) => e,
                            other => UserFacing::LoginFailed(other.to_string()),
                        });
                    }
                }
            }
            Internal::Observed(volume) => self.coalescer.on_remote_observed(volume),
        }
    }

    async fn on_timer(&mut self) {
        if let Some(volume) = self.coalescer.take_send(Instant::now()) {
            self.send_volume(volume).await;
        }
        if self.reconcile_at.is_some_and(|at| Instant::now() >= at) {
            self.reconcile_at = None;
            self.refresh(false).await;
        }
    }

    async fn send_volume(&mut self, volume: u8) {
        if !self.logged_in {
            self.emit_error(UserFacing::NotLoggedIn);
            // Confirm it locally so we do not retry a request that cannot work.
            self.coalescer.on_sent_ok(volume, Instant::now());
            return;
        }

        let error = match self.client.volume(volume, None).await {
            Ok(()) => return self.on_send_ok(volume),
            Err(e) => classify(e).await,
        };

        // A 401 here means the access token died mid-burst: refresh once and
        // retry the same value once.
        if error == UserFacing::NotLoggedIn && !self.auth_retry_used {
            self.auth_retry_used = true;
            if let Err(e) = self.client.refresh_token().await {
                log::warn!("cannot refresh the Spotify token: {e}");
                self.coalescer.abandon();
                self.set_logged_out();
                self.emit_error(UserFacing::NotLoggedIn);
                return;
            }
            match self.client.volume(volume, None).await {
                Ok(()) => return self.on_send_ok(volume),
                Err(e) => {
                    let retried = classify(e).await;
                    self.on_send_err(retried);
                    return;
                }
            }
        }

        self.on_send_err(error);
    }

    fn on_send_ok(&mut self, volume: u8) {
        let now = Instant::now();
        self.coalescer.on_sent_ok(volume, now);
        self.consecutive_failures = 0;
        self.auth_retry_used = false;
        self.last_synced = Some(now);
        // One timer, pushed back by every send, so it fires after the burst.
        self.reconcile_at = Some(now + RECONCILE_AFTER);
        self.emit(SpotifyEvent::VolumeApplied(volume));
    }

    fn on_send_err(&mut self, error: UserFacing) {
        let now = Instant::now();
        match &error {
            // Spotify told us exactly how long to wait, so this is not a
            // failure to give up on.
            UserFacing::RateLimited { retry_after } => {
                self.coalescer.on_sent_err(*retry_after, now);
            }
            _ => {
                self.consecutive_failures += 1;
                if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    log::warn!("dropping the pending volume after {error}");
                    self.coalescer.abandon();
                } else {
                    self.coalescer.on_sent_err(DEFAULT_BACKOFF, now);
                }
            }
        }
        if error == UserFacing::NotLoggedIn {
            self.set_logged_out();
        }
        self.emit_error(error);
    }

    /// Read the current playback. `user_triggered` distinguishes an explicit
    /// `Refresh` (which may report "no active device") from the background
    /// reconciles, which stay silent.
    async fn refresh(&mut self, user_triggered: bool) {
        if !self.logged_in {
            if user_triggered {
                self.emit_error(UserFacing::NotLoggedIn);
            }
            return;
        }

        match self
            .client
            .current_playback(None, Some([&AdditionalType::Track]))
            .await
        {
            Ok(Some(context)) => {
                let snapshot = snapshot_of(&context);
                if let Some(volume) = snapshot.volume {
                    self.coalescer.on_remote_observed(volume);
                }
                self.last_synced = Some(Instant::now());
                self.consecutive_failures = 0;
                self.auth_retry_used = false;
                self.emit(SpotifyEvent::Playback(snapshot));
            }
            Ok(None) => {
                log::debug!("no active playback");
                if user_triggered && self.user_interacted {
                    self.emit_error(UserFacing::NoActiveDevice);
                }
            }
            Err(e) => {
                let error = classify(e).await;
                if error == UserFacing::NotLoggedIn {
                    self.set_logged_out();
                }
                if user_triggered || error == UserFacing::NotLoggedIn {
                    self.emit_error(error);
                } else {
                    log::debug!("background refresh failed: {error}");
                }
            }
        }
    }

    /// Read the playback state without blocking the actor loop. The result is
    /// reported straight to the UI; only the coalescer baseline comes back
    /// through the internal channel.
    fn spawn_background_refresh(&mut self, itx: &mpsc::UnboundedSender<Internal>) {
        // Count it as synced right away so a long burst spawns exactly one.
        self.last_synced = Some(Instant::now());
        let client = self.client.clone();
        let sink = Arc::clone(&self.sink);
        let itx = itx.clone();
        tokio::spawn(async move {
            match client
                .current_playback(None, Some([&AdditionalType::Track]))
                .await
            {
                Ok(Some(context)) => {
                    let snapshot = snapshot_of(&context);
                    if let Some(volume) = snapshot.volume {
                        let _ = itx.send(Internal::Observed(volume));
                    }
                    sink(SpotifyEvent::Playback(snapshot));
                }
                Ok(None) => log::debug!("background reconcile: no active playback"),
                Err(e) => log::debug!("background reconcile failed: {}", classify(e).await),
            }
        });
    }
}

/// rspotify 0.16 models the device without a `supports_volume` flag, so derive
/// it: a restricted device rejects the Web API player commands, and a device
/// that reports no volume has none to set.
fn snapshot_of(context: &CurrentPlaybackContext) -> PlaybackSnapshot {
    let device = &context.device;
    let volume = device.volume_percent.map(|v| v.min(100) as u8);
    PlaybackSnapshot {
        volume,
        device_name: device.name.clone(),
        device_id: device.id.clone(),
        supports_volume: volume.is_some() && !device.is_restricted,
        is_playing: context.is_playing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::{spawn_spotify, SpotifyHandle};
    use std::path::Path;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A one-request-at-a-time HTTP server that answers everything with the
    /// same canned response and records the request lines it saw.
    struct MockApi {
        base_url: String,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl MockApi {
        async fn start(status: &'static str, body: &'static str) -> Self {
            Self::start_with_headers(status, &[], body).await
        }

        /// `status` is an HTTP status line without the version ("404 Not
        /// Found"); `headers` are extra `Name: value` pairs.
        async fn start_with_headers(
            status: &'static str,
            headers: &'static [&'static str],
            body: &'static str,
        ) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
            let port = listener.local_addr().expect("addr").port();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let recorder = Arc::clone(&seen);

            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let mut buffer = [0u8; 2048];
                    let read = stream.read(&mut buffer).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                    if let Some(line) = request.lines().next() {
                        if let Ok(mut seen) = recorder.lock() {
                            seen.push(line.to_owned());
                        }
                    }
                    let mut response = format!("HTTP/1.1 {status}\r\n");
                    for header in headers {
                        response.push_str(header);
                        response.push_str("\r\n");
                    }
                    response.push_str(&format!(
                        "Content-Type: application/json\r\nContent-Length: {len}\r\n\
                         Connection: close\r\n\r\n{body}",
                        len = body.len()
                    ));
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                }
            });

            Self {
                // rspotify appends endpoint paths straight onto this.
                base_url: format!("http://127.0.0.1:{port}/v1/"),
                seen,
            }
        }

        fn requests(&self) -> Vec<String> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .unwrap_or_default()
        }
    }

    /// A client wired to the mock server with a token that never expires.
    fn mock_client(base_url: &str, cache_path: &Path) -> AuthCodePkceSpotify {
        let cfg = SpotifyConfig {
            client_id: "test-client".to_owned(),
            redirect_port: 8888,
            cache_path: cache_path.to_path_buf(),
        };
        let mut client = auth::build_client(&cfg);
        client.config.api_base_url = base_url.to_owned();
        // Never write a cache file from a test.
        client.config.token_cached = false;
        client.config.token_refreshing = false;
        let json = r#"{"access_token":"at","expires_in":3600,"expires_at":"2999-01-01T00:00:00Z","refresh_token":"rt","scope":"user-modify-playback-state user-read-playback-state"}"#;
        let token = serde_json::from_str(json).expect("token json");
        client.token = Arc::new(rspotify::sync::Mutex::new(Some(token)));
        client
    }

    #[tokio::test]
    async fn a_404_from_the_volume_endpoint_means_no_active_device() {
        let body = r#"{"error":{"status":404,"message":"Device not found"}}"#;
        let api = MockApi::start("404 Not Found", body).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let client = mock_client(&api.base_url, &dir.path().join("token.json"));

        let error = client
            .volume(42, None)
            .await
            .expect_err("the mock always answers 404");
        assert_eq!(classify(error).await, UserFacing::NoActiveDevice);

        let requests = api.requests();
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert!(
            requests[0].starts_with("PUT /v1/me/player/volume?volume_percent=42"),
            "{requests:?}"
        );
    }

    #[tokio::test]
    async fn a_429_from_the_volume_endpoint_carries_the_retry_after() {
        let api =
            MockApi::start_with_headers("429 Too Many Requests", &["Retry-After: 5"], "{}").await;
        let dir = tempfile::tempdir().expect("tempdir");
        let client = mock_client(&api.base_url, &dir.path().join("token.json"));

        let error = client.volume(10, None).await.expect_err("429");
        assert_eq!(
            classify(error).await,
            UserFacing::RateLimited {
                retry_after: Duration::from_secs(5)
            }
        );
    }

    #[tokio::test]
    async fn an_empty_playback_body_is_not_an_error() {
        let api = MockApi::start("204 No Content", "").await;
        let dir = tempfile::tempdir().expect("tempdir");
        let client = mock_client(&api.base_url, &dir.path().join("token.json"));

        let playback = client
            .current_playback(None, Some([&AdditionalType::Track]))
            .await
            .expect("204 is a success");
        assert!(playback.is_none());
    }

    #[tokio::test]
    async fn playback_maps_onto_a_snapshot() {
        let body = r#"{
            "device": {"id":"dev1","is_active":true,"is_private_session":false,
                       "is_restricted":false,"name":"Study speaker","type":"Speaker",
                       "volume_percent":37},
            "repeat_state":"off","shuffle_state":false,"context":null,
            "timestamp":1700000000000,"progress_ms":1000,"is_playing":true,
            "item":null,"currently_playing_type":"track","actions":{"disallows":{}}
        }"#;
        let api = MockApi::start("200 OK", body).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let client = mock_client(&api.base_url, &dir.path().join("token.json"));

        let context = client
            .current_playback(None, Some([&AdditionalType::Track]))
            .await
            .expect("playback")
            .expect("some playback");
        let snapshot = snapshot_of(&context);
        assert_eq!(
            snapshot,
            PlaybackSnapshot {
                volume: Some(37),
                device_name: "Study speaker".to_owned(),
                device_id: Some("dev1".to_owned()),
                supports_volume: true,
                is_playing: true,
            }
        );
    }

    #[test]
    fn a_restricted_device_does_not_support_volume() {
        let body = r#"{
            "device": {"id":null,"is_active":true,"is_private_session":false,
                       "is_restricted":true,"name":"Car","type":"Automobile",
                       "volume_percent":null},
            "repeat_state":"off","shuffle_state":false,"context":null,
            "timestamp":1700000000000,"progress_ms":null,"is_playing":false,
            "item":null,"currently_playing_type":"unknown","actions":{"disallows":{}}
        }"#;
        let context: CurrentPlaybackContext = serde_json::from_str(body).expect("context");
        let snapshot = snapshot_of(&context);
        assert!(!snapshot.supports_volume);
        assert_eq!(snapshot.volume, None);
        assert_eq!(snapshot.device_id, None);
    }

    /// The playback body the mock answers with; also fine as a PUT response,
    /// which `volume()` ignores.
    const PLAYBACK_AT_37: &str = r#"{
        "device": {"id":"dev1","is_active":true,"is_private_session":false,
                   "is_restricted":false,"name":"Study speaker","type":"Speaker",
                   "volume_percent":37},
        "repeat_state":"off","shuffle_state":false,"context":null,
        "timestamp":1700000000000,"progress_ms":1000,"is_playing":true,
        "item":null,"currently_playing_type":"track","actions":{"disallows":{}}
    }"#;

    /// An `ActorState` wired to a mock server, pretending to be logged in.
    fn logged_in_state(
        api: &MockApi,
        dir: &tempfile::TempDir,
    ) -> (ActorState, Arc<Mutex<Vec<SpotifyEvent>>>) {
        let cache_path = dir.path().join("token.json");
        let (sink, events) = recording_sink();
        let cfg = SpotifyConfig {
            client_id: "test-client".to_owned(),
            redirect_port: 8888,
            cache_path: cache_path.clone(),
        };
        let mut state = ActorState::new(cfg, sink);
        state.client = mock_client(&api.base_url, &cache_path);
        state.logged_in = true;
        state.user_interacted = true;
        // A fresh baseline, as if a reconcile had just run.
        state.last_synced = Some(Instant::now());
        state.coalescer.on_remote_observed(37);
        if let Ok(mut events) = events.lock() {
            events.clear();
        }
        (state, events)
    }

    #[tokio::test]
    async fn a_burst_of_ticks_becomes_one_put_and_arms_the_reconcile() {
        let api = MockApi::start("200 OK", PLAYBACK_AT_37).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut state, events) = logged_in_state(&api, &dir);
        let (itx, _irx) = mpsc::unbounded_channel();

        for volume in 38..=48 {
            state.on_local_volume(volume, &itx);
        }
        assert!(
            api.requests().is_empty(),
            "nothing may go out during a burst"
        );

        // Wait out the quiet period, then run the timer the loop would run.
        wait_until(state.next_wakeup()).await;
        state.on_timer().await;

        let requests = api.requests();
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert!(
            requests[0].starts_with("PUT /v1/me/player/volume?volume_percent=48"),
            "only the final value is sent: {requests:?}"
        );
        assert_eq!(
            events.lock().map(|e| e.clone()).unwrap_or_default(),
            vec![SpotifyEvent::VolumeApplied(48)]
        );
        assert!(state.reconcile_at.is_some(), "the reconcile must be armed");
        assert_eq!(state.next_wakeup(), state.reconcile_at);

        // Fire the reconcile: one GET, one playback snapshot, timer cleared.
        state.reconcile_at = Some(Instant::now());
        state.on_timer().await;
        let requests = api.requests();
        assert_eq!(requests.len(), 2, "{requests:?}");
        assert!(requests[1].starts_with("GET /v1/me/player"), "{requests:?}");
        assert_eq!(state.reconcile_at, None);
        let seen = events.lock().map(|e| e.clone()).unwrap_or_default();
        assert!(
            matches!(seen.last(), Some(SpotifyEvent::Playback(snapshot)) if snapshot.volume == Some(37)),
            "{seen:?}"
        );
        assert_eq!(state.next_wakeup(), None, "nothing left to do");
    }

    #[tokio::test]
    async fn a_persistent_failure_stops_after_two_attempts() {
        let body = r#"{"error":{"status":403,"message":"Player command failed: Restriction violated","reason":"UNKNOWN"}}"#;
        let api = MockApi::start("403 Forbidden", body).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut state, events) = logged_in_state(&api, &dir);
        let (itx, _irx) = mpsc::unbounded_channel();

        state.on_local_volume(50, &itx);
        wait_until(state.next_wakeup()).await;
        state.on_timer().await;
        assert_eq!(api.requests().len(), 1);
        assert!(state.next_wakeup().is_some(), "first failure backs off");

        // Second (and last) attempt.
        wait_until(state.next_wakeup()).await;
        state.on_timer().await;
        assert_eq!(api.requests().len(), 2);
        assert_eq!(
            state.next_wakeup(),
            None,
            "the pending volume must be dropped, not retried forever"
        );

        // One error per attempt at most: the backoff equals the throttle
        // window, so the retry is allowed to speak up again.
        let seen = events.lock().map(|e| e.clone()).unwrap_or_default();
        assert!(seen.len() <= 2, "{seen:?}");
        assert!(
            seen.iter()
                .all(|e| *e == SpotifyEvent::Error(UserFacing::VolumeControlNotAllowed)),
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn a_rate_limit_waits_for_the_retry_after_header() {
        let api =
            MockApi::start_with_headers("429 Too Many Requests", &["Retry-After: 1"], "{}").await;
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut state, events) = logged_in_state(&api, &dir);
        let (itx, _irx) = mpsc::unbounded_channel();

        state.on_local_volume(60, &itx);
        wait_until(state.next_wakeup()).await;
        let before = Instant::now();
        state.on_timer().await;

        let wakeup = state.next_wakeup().expect("a 429 re-arms the deadline");
        let backoff = wakeup.duration_since(before);
        assert!(
            backoff >= Duration::from_millis(900) && backoff <= Duration::from_millis(1600),
            "Retry-After: 1 should mean about a second, got {backoff:?}"
        );
        let seen = events.lock().map(|e| e.clone()).unwrap_or_default();
        assert_eq!(
            seen,
            vec![SpotifyEvent::Error(UserFacing::RateLimited {
                retry_after: Duration::from_secs(1)
            })]
        );
        // A 429 is not counted as a failure to give up on.
        assert_eq!(state.consecutive_failures, 0);
    }

    /// Collects everything the actor emits.
    fn recording_sink() -> (EventSink, Arc<Mutex<Vec<SpotifyEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&events);
        let sink: EventSink = Arc::new(move |event| {
            if let Ok(mut events) = recorder.lock() {
                events.push(event);
            }
        });
        (sink, events)
    }

    fn wait_for(
        events: &Arc<Mutex<Vec<SpotifyEvent>>>,
        predicate: impl Fn(&[SpotifyEvent]) -> bool,
    ) -> Vec<SpotifyEvent> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let snapshot = events.lock().map(|e| e.clone()).unwrap_or_default();
            if predicate(&snapshot) {
                return snapshot;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting; saw {snapshot:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Spawns the real actor thread (no client ID, so it never talks HTTP).
    fn spawn_unconfigured() -> (
        SpotifyHandle,
        Arc<Mutex<Vec<SpotifyEvent>>>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let (sink, events) = recording_sink();
        let handle = spawn_spotify(
            SpotifyConfig {
                client_id: String::new(),
                redirect_port: 8888,
                cache_path: dir.path().join("token.json"),
            },
            sink,
        );
        (handle, events, dir)
    }

    #[test]
    fn an_unconfigured_actor_reports_logged_out_and_asks_for_a_client_id() {
        let (handle, events, _dir) = spawn_unconfigured();
        wait_for(&events, |e| {
            e.contains(&SpotifyEvent::Auth(AuthState::LoggedOut))
        });

        // A burst of ticks must produce exactly one complaint.
        for volume in 40..50 {
            handle.set_volume(volume);
        }
        let expected = SpotifyEvent::Error(UserFacing::LoginFailed(
            "Set your Spotify client ID in Settings".to_owned(),
        ));
        let seen = wait_for(&events, |e| e.contains(&expected));
        assert_eq!(
            seen.iter().filter(|e| **e == expected).count(),
            1,
            "one error per burst: {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, SpotifyEvent::VolumeApplied(_))),
            "nothing can be applied without a client ID: {seen:?}"
        );

        handle.send(SpotifyCmd::Shutdown);
    }

    #[test]
    fn refresh_without_a_login_reports_not_logged_in() {
        let (handle, events, _dir) = spawn_unconfigured();
        wait_for(&events, |e| {
            e.contains(&SpotifyEvent::Auth(AuthState::LoggedOut))
        });

        handle.send(SpotifyCmd::Refresh);
        wait_for(&events, |e| {
            e.contains(&SpotifyEvent::Error(UserFacing::NotLoggedIn))
        });

        handle.send(SpotifyCmd::Shutdown);
    }

    #[tokio::test]
    async fn read_volume_reports_a_missing_device_but_refresh_stays_quiet() {
        // 204: logged in, but nothing is playing anywhere.
        let api = MockApi::start("204 No Content", "").await;
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut state, events) = logged_in_state(&api, &dir);
        let (itx, _irx) = mpsc::unbounded_channel();
        // As if the app had only just started.
        state.user_interacted = false;

        // A refresh after a login must not accuse the user of anything.
        state.handle_cmd(SpotifyCmd::Refresh, &itx).await;
        assert_eq!(events.lock().expect("events").as_slice(), &[]);

        // A held knob tick is waiting for this answer, so it has to be told
        // that no answer is coming.
        state.handle_cmd(SpotifyCmd::ReadVolume, &itx).await;
        assert_eq!(
            events.lock().expect("events").as_slice(),
            &[SpotifyEvent::Error(UserFacing::NoActiveDevice)]
        );
        assert_eq!(api.requests().len(), 2, "both commands read the state");
    }

    #[test]
    fn login_without_a_client_id_fails_immediately() {
        let (handle, events, _dir) = spawn_unconfigured();
        handle.send(SpotifyCmd::Login);
        let seen = wait_for(&events, |e| {
            e.iter()
                .any(|e| matches!(e, SpotifyEvent::Auth(AuthState::Failed(_))))
        });
        assert!(
            seen.iter().any(|e| matches!(
                e,
                SpotifyEvent::Error(UserFacing::LoginFailed(msg)) if msg.contains("client ID")
            )),
            "{seen:?}"
        );

        handle.send(SpotifyCmd::Shutdown);
    }
}
