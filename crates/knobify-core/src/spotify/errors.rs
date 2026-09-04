//! Mapping of rspotify errors to messages the OSD can show.

use std::fmt;
use std::time::Duration;

use rspotify::http::HttpError;
use rspotify::ClientError;

/// Fallback wait when Spotify rate-limits us without a usable `Retry-After`.
const DEFAULT_RETRY_AFTER: u64 = 2;
/// Never sit on a pending volume change for longer than this.
const MAX_RETRY_AFTER: u64 = 60;

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
    RateLimited {
        retry_after: Duration,
    },
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
                write!(
                    f,
                    "Spotify rate limit, retrying in {}s",
                    retry_after.as_secs().max(1)
                )
            }
            UserFacing::Offline => f.write_str("Cannot reach Spotify"),
            UserFacing::LoginFailed(msg) | UserFacing::Other(msg) => f.write_str(msg),
        }
    }
}

/// The interesting parts of a Spotify error body.
///
/// Player endpoints answer with
/// `{"error":{"status":403,"message":"Player command failed: Restriction
/// violated","reason":"UNKNOWN"}}`, while the accounts service answers with
/// `{"error":"invalid_grant","error_description":"..."}`.
#[derive(Debug, Default, PartialEq, Eq)]
struct ApiError {
    message: String,
    reason: Option<String>,
}

fn parse_api_error(body: &str) -> ApiError {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return ApiError {
            message: body.trim().chars().take(200).collect(),
            reason: None,
        };
    };

    match value.get("error") {
        // `{"error": {"message": ..., "reason": ...}}`
        Some(serde_json::Value::Object(error)) => ApiError {
            message: error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            reason: error
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        },
        // `{"error": "invalid_grant", "error_description": "..."}`
        Some(serde_json::Value::String(code)) => {
            let description = value
                .get("error_description")
                .and_then(serde_json::Value::as_str);
            ApiError {
                message: match description {
                    Some(description) => format!("{code}: {description}"),
                    None => code.clone(),
                },
                reason: Some(code.clone()),
            }
        }
        _ => ApiError::default(),
    }
}

/// Map an HTTP status plus response body to a [`UserFacing`] error. Pure, so
/// the interesting cases are unit-testable; [`classify`] only extracts the
/// status, the `Retry-After` header and the body and delegates here.
pub fn classify_status(status: u16, body: &str, retry_after: Option<u64>) -> UserFacing {
    let ApiError { message, reason } = parse_api_error(body);
    let reason = reason.unwrap_or_default();

    match status {
        401 => UserFacing::NotLoggedIn,
        403 => {
            if message.contains("Restriction violated") || reason == "VOLUME_CONTROL_DISALLOW" {
                UserFacing::VolumeControlNotAllowed
            } else if message.to_lowercase().contains("premium")
                || reason.eq_ignore_ascii_case("PREMIUM_REQUIRED")
            {
                UserFacing::PremiumRequired
            } else {
                UserFacing::Other(fallback(&message, status))
            }
        }
        404 => UserFacing::NoActiveDevice,
        429 => UserFacing::RateLimited {
            retry_after: Duration::from_secs(
                retry_after
                    .unwrap_or(DEFAULT_RETRY_AFTER)
                    .clamp(1, MAX_RETRY_AFTER),
            ),
        },
        _ => UserFacing::Other(fallback(&message, status)),
    }
}

fn fallback(message: &str, status: u16) -> String {
    if message.is_empty() {
        format!("Spotify returned HTTP {status}")
    } else {
        message.to_owned()
    }
}

/// Classify an rspotify error by HTTP status and response body.
pub async fn classify(error: ClientError) -> UserFacing {
    match error {
        ClientError::Http(http) => match *http {
            HttpError::StatusCode(response) => {
                let status = response.status().as_u16();
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<u64>().ok());
                let body = response.text().await.unwrap_or_default();
                log::debug!("spotify http {status}: {body}");
                classify_status(status, &body, retry_after)
            }
            HttpError::Client(transport) => {
                log::debug!("spotify transport error: {transport}");
                UserFacing::Offline
            }
        },
        // rspotify raises this when there is no token at all, which for the
        // user means exactly "not logged in".
        ClientError::InvalidToken => UserFacing::NotLoggedIn,
        other => UserFacing::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESTRICTION: &str = r#"{"error":{"status":403,"message":"Player command failed: Restriction violated","reason":"UNKNOWN"}}"#;

    #[test]
    fn unauthorized_means_not_logged_in() {
        let body = r#"{"error":{"status":401,"message":"The access token expired"}}"#;
        assert_eq!(classify_status(401, body, None), UserFacing::NotLoggedIn);
    }

    #[test]
    fn restriction_violated_means_volume_control_not_allowed() {
        assert_eq!(
            classify_status(403, RESTRICTION, None),
            UserFacing::VolumeControlNotAllowed
        );
    }

    #[test]
    fn volume_control_disallow_reason_means_volume_control_not_allowed() {
        let body = r#"{"error":{"status":403,"message":"Player command failed: nope","reason":"VOLUME_CONTROL_DISALLOW"}}"#;
        assert_eq!(
            classify_status(403, body, None),
            UserFacing::VolumeControlNotAllowed
        );
    }

    #[test]
    fn premium_is_matched_case_insensitively() {
        let body = r#"{"error":{"status":403,"message":"Player command failed: Premium required","reason":"PREMIUM_REQUIRED"}}"#;
        assert_eq!(
            classify_status(403, body, None),
            UserFacing::PremiumRequired
        );

        let lowercase = r#"{"error":{"status":403,"message":"this needs a premium account"}}"#;
        assert_eq!(
            classify_status(403, lowercase, None),
            UserFacing::PremiumRequired
        );
    }

    #[test]
    fn other_forbidden_keeps_the_spotify_message() {
        let body = r#"{"error":{"status":403,"message":"Player command failed: No active device","reason":"NO_ACTIVE_DEVICE"}}"#;
        assert_eq!(
            classify_status(403, body, None),
            UserFacing::Other("Player command failed: No active device".into())
        );
    }

    #[test]
    fn forbidden_without_a_body_still_says_something() {
        assert_eq!(
            classify_status(403, "", None),
            UserFacing::Other("Spotify returned HTTP 403".into())
        );
    }

    #[test]
    fn not_found_means_no_active_device() {
        assert_eq!(classify_status(404, "", None), UserFacing::NoActiveDevice);
    }

    #[test]
    fn rate_limit_uses_the_retry_after_header() {
        assert_eq!(
            classify_status(429, "", Some(7)),
            UserFacing::RateLimited {
                retry_after: Duration::from_secs(7)
            }
        );
    }

    #[test]
    fn rate_limit_defaults_to_two_seconds() {
        assert_eq!(
            classify_status(429, "", None),
            UserFacing::RateLimited {
                retry_after: Duration::from_secs(DEFAULT_RETRY_AFTER)
            }
        );
    }

    #[test]
    fn rate_limit_is_clamped() {
        assert_eq!(
            classify_status(429, "", Some(0)),
            UserFacing::RateLimited {
                retry_after: Duration::from_secs(1)
            }
        );
        assert_eq!(
            classify_status(429, "", Some(3600)),
            UserFacing::RateLimited {
                retry_after: Duration::from_secs(MAX_RETRY_AFTER)
            }
        );
    }

    #[test]
    fn unknown_statuses_fall_back_to_other() {
        assert_eq!(
            classify_status(502, "", None),
            UserFacing::Other("Spotify returned HTTP 502".into())
        );
        let body = r#"{"error":{"status":500,"message":"Server error."}}"#;
        assert_eq!(
            classify_status(500, body, None),
            UserFacing::Other("Server error.".into())
        );
    }

    #[test]
    fn accounts_service_errors_are_readable() {
        let body = r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#;
        assert_eq!(
            classify_status(400, body, None),
            UserFacing::Other("invalid_grant: Refresh token revoked".into())
        );
    }

    #[test]
    fn a_non_json_body_is_truncated_not_dropped() {
        let body = "<html>gateway timeout</html>";
        assert_eq!(
            classify_status(504, body, None),
            UserFacing::Other(body.into())
        );
    }

    #[tokio::test]
    async fn non_http_client_errors_become_other() {
        let error = ClientError::CacheFile("cannot read token.json".into());
        let message = error.to_string();
        assert_eq!(classify(error).await, UserFacing::Other(message));
    }

    #[tokio::test]
    async fn invalid_token_becomes_not_logged_in() {
        assert_eq!(
            classify(ClientError::InvalidToken).await,
            UserFacing::NotLoggedIn
        );
    }
}
