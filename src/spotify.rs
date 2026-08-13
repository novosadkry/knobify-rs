use crate::config;
use anyhow::{bail, Context, Result};
use std::{
    env, net::SocketAddr, path::PathBuf, sync::Arc
};
use tokio::{
    net::{TcpListener, TcpStream},
    io::{BufReader, AsyncBufReadExt, AsyncWriteExt}
};
use rspotify::{
    model::AdditionalType,
    prelude::*,
    scopes,
    sync::Mutex,
    AuthCodeSpotify,
    Config,
    Credentials,
    OAuth
};

#[derive(Default)]
pub struct Spotify {
    client: AuthCodeSpotify,
    volume: u8,
    authenticated: bool
}

impl Spotify {
    /// Logs in interactively, opening a browser window if there's no usable
    /// cached session. Used for the tray "Login" action.
    pub async fn login() -> Result<Spotify> {
        let client = oauth_client(true).await?;
        Spotify::from_client(client).await
    }

    /// Tries to restore a previously logged-in session from the token cache
    /// without ever opening a browser. Used to restore the session on startup.
    pub async fn from_cache() -> Result<Spotify> {
        let client = oauth_client(false).await?;
        Spotify::from_client(client).await
    }

    async fn from_client(client: AuthCodeSpotify) -> Result<Spotify> {
        // A failure to fetch the current playback (e.g. no active device, or a
        // transient network error) shouldn't prevent logging in - just fall
        // back to a default volume.
        let volume = client
            .current_playback(None, Some([&AdditionalType::Track])).await
            .ok()
            .flatten()
            .and_then(|playback| playback.device.volume_percent)
            .unwrap_or(50) as u8;

        Ok(Spotify { client, volume, authenticated: true })
    }

    pub async fn volume_up(&mut self) -> Result<()> {
        if !self.authenticated {
            return Ok(());
        }

        let increment = config::get_volume_increment();
        self.volume = self.volume.saturating_add(increment).min(100);
        self.client.volume(self.volume, None).await?;

        Ok(())
    }

    pub async fn volume_down(&mut self) -> Result<()> {
        if !self.authenticated {
            return Ok(());
        }

        let increment = config::get_volume_increment();
        self.volume = self.volume.saturating_sub(increment);
        self.client.volume(self.volume, None).await?;

        Ok(())
    }
}

/// Returns a stable, writable location for the cached Spotify token that
/// doesn't depend on the process's current working directory (which can
/// vary depending on how the tray app was launched, e.g. on startup).
fn token_cache_path() -> PathBuf {
    let base = env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);

    let dir = base.join("Knobify");
    let _ = std::fs::create_dir_all(&dir);

    dir.join("spotify_token_cache.json")
}

/// Builds an authenticated client. If `interactive` is true and there's no
/// usable cached session, opens a browser window to log in. Otherwise, only
/// a valid cached session is used and an error is returned if none exists.
async fn oauth_client(interactive: bool) -> Result<AuthCodeSpotify> {
    let oauth = OAuth {
        redirect_uri: String::from("http://127.0.0.1:8888/callback"),
        scopes: scopes!(
            "streaming",
            "playlist-read-collaborative",
            "playlist-read-private",
            "playlist-modify-private",
            "playlist-modify-public",
            "user-follow-read",
            "user-follow-modify",
            "user-library-modify",
            "user-library-read",
            "user-modify-playback-state",
            "user-read-currently-playing",
            "user-read-playback-state",
            "user-read-playback-position",
            "user-read-private",
            "user-read-recently-played"
        ),
        ..Default::default()
    };

    let creds = Credentials::from_env()
        .context("Couldn't load environment variables RSPOTIFY_CLIENT_ID and RSPOTIFY_CLIENT_SECRET")?;

    let mut spotify = AuthCodeSpotify::with_config(
        creds,
        oauth,
        Config {
            token_cached: true,
            token_refreshing: true,
            cache_path: token_cache_path(),
            ..Default::default()
        }
    );

    // allow_expired=true: an expired-but-cached token is still useful, since
    // it carries a refresh token that lets subsequent API calls silently
    // refresh it (token_refreshing above) instead of forcing a fresh login.
    match spotify.read_token_cache(true).await.ok().flatten() {
        Some(token) => {
            spotify.token = Arc::new(Mutex::new(Some(token)));
        },
        None if interactive => {
            let url = spotify.get_authorize_url(false)?;
            let code = get_code_from_user(&spotify, url.as_str()).await
                .context("Couldn't acquire auth code from the user")?;

            spotify.request_token(code.as_str()).await?;
            spotify.write_token_cache().await?;
        },
        None => bail!("No cached Spotify session available"),
    };

    Ok(spotify)
}

async fn get_code_from_user(spotify: &AuthCodeSpotify, url: &str) -> Result<String> {
    match webbrowser::open(url) {
        Ok(_) => println!("Please proceed to log-in in your browser."),
        Err(_) => eprintln!(
            "Unable to open the URL in your browser. \
            Please navigate here manually: {}", url
        ),
    }

    let addr = "127.0.0.1:8888".parse::<SocketAddr>()?;
    match TcpListener::bind(&addr).await {
        Ok(listener) => {
            let (mut stream, _) = listener.accept().await?;
            let (reader, _) = stream.split();

            let mut buf = String::new();
            let mut buf_reader = BufReader::new(reader);
            buf_reader.read_line(&mut buf).await?;

            let header = buf
                .split_whitespace()
                .collect::<Vec<&str>>();

            let code = spotify
                .parse_response_code(format!("{}{}", "http://localhost:8888", header[1]).as_str())
                .context("Unable to parse the response code")?;

            respond_with_success(&mut stream).await?;

            Ok(code)
        },

        Err(_) => {
            println!("Please enter the URL you were redirected to: ");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

            let code = spotify
                .parse_response_code(&input)
                .context("Unable to parse the response code")?;

            Ok(code)
        }
    }
}

async fn respond_with_success(stream: &mut TcpStream) -> Result<()> {
    let contents = String::from("<script>window.close();</script>");
    let response = format!("HTTP/1.1 200 OK\r\n\r\n{}", contents);

    stream.write(response.as_bytes()).await?;
    stream.flush().await?;

    return Ok(())
}
