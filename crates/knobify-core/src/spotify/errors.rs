//! Mapping of rspotify errors to messages the OSD can show.

use std::fmt;
use std::time::Duration;

use rspotify::ClientError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserFacing {
    NotLoggedIn,
    /// 404: no active Spotify device.
    NoActiveDevice,
    /// 403 "Restriction violated": the device does not allow remote volume control.
    VolumeControlNotAllowed,
    /// 403: player endpoints require Spotify Premium.
    PremiumRequired,
    /// 429 with `Retry-After`.
    RateLimited { retry_after: Duration },
    /// Transport failure (no network, DNS, TLS).
    Offline,
    LoginFailed(String),
    Other(String),
}

impl UserFacing {
    /// Short title for the OSD.
    pub fn title(&self) -> &'static str {
        match self {
            UserFacing::NotLoggedIn => "Not logged in",
            UserFacing::NoActiveDevice => "No active Spotify device",
            UserFacing::VolumeControlNotAllowed => "Volume control not allowed",
            UserFacing::PremiumRequired => "Spotify Premium required",
            UserFacing::RateLimited { .. } => "Slow down",
            UserFacing::Offline => "Offline",
            UserFacing::LoginFailed(_) => "Login failed",
            UserFacing::Other(_) => "Spotify error",
        }
    }
}

impl fmt::Display for UserFacing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UserFacing::NotLoggedIn => f.write_str("Log in to Spotify from the tray menu"),
            UserFacing::NoActiveDevice => f.write_str("Start playback on a device first"),
            UserFacing::VolumeControlNotAllowed => {
                f.write_str("This device does not accept remote volume changes")
            }
            UserFacing::PremiumRequired => {
                f.write_str("Controlling playback needs a Premium account")
            }
            UserFacing::RateLimited { retry_after } => {
                write!(f, "Spotify rate limit, retrying in {}s", retry_after.as_secs().max(1))
            }
            UserFacing::Offline => f.write_str("Cannot reach Spotify"),
            UserFacing::LoginFailed(msg) | UserFacing::Other(msg) => f.write_str(msg),
        }
    }
}

/// Classify an rspotify error by HTTP status and response body.
pub async fn classify(error: ClientError) -> UserFacing {
    let _ = error;
    todo!("WP1: map ClientError (status + body) to UserFacing")
}
