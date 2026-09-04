//! User settings: TOML file in the per-user config directory.
//!
//! Every struct uses `#[serde(default)]` so that a partial or older file still
//! loads. Saving is atomic (write to a temporary file, then rename).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::keycode::KeyCode;

pub const CONFIG_VERSION: u32 = 1;
pub const APP_DIR_NAME: &str = "knobify";
pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const TOKEN_FILE_NAME: &str = "token.json";

pub const MIN_STEP: u8 = 1;
pub const MAX_STEP: u8 = 25;
pub const MIN_OSD_DURATION_MS: u64 = 300;
pub const MAX_OSD_DURATION_MS: u64 = 10_000;
pub const MIN_SYNC_DELAY_MS: u64 = 250;
pub const MAX_SYNC_DELAY_MS: u64 = 15_000;
/// Measured against the real API: `GET /me/player` reported a volume this app
/// had just set only 0.4 s to 2.4 s after Spotify accepted the change, so
/// anything it says inside that window may still be the previous volume.
/// The default keeps headroom over the worst case that was observed.
pub const DEFAULT_SYNC_DELAY_MS: u64 = 3000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// Spotify application client ID (PKCE flow, no secret).
    pub client_id: String,
    /// Loopback port for the OAuth redirect: `http://127.0.0.1:{port}/callback`.
    pub redirect_port: u16,
    /// Volume change per knob tick, in percent.
    pub step: u8,
    /// How long Spotify may take to report a volume change this app made.
    ///
    /// Nothing Spotify says about the volume is believed inside this window
    /// after a change of our own, because it is likely to be the *previous*
    /// volume; and a baseline this old is re-read before the knob works from
    /// it. Raise it if a turn of the knob gets pulled back to an older value.
    pub sync_delay_ms: u64,
    pub bindings: Bindings,
    pub osd: OsdSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            client_id: String::new(),
            redirect_port: 8888,
            step: 5,
            sync_delay_ms: DEFAULT_SYNC_DELAY_MS,
            bindings: Bindings::default(),
            osd: OsdSettings::default(),
        }
    }
}

impl Settings {
    /// [`Self::sync_delay_ms`] as a `Duration`.
    pub fn sync_delay(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.sync_delay_ms)
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.redirect_port)
    }

    /// Clamp every numeric field into its supported range.
    pub fn sanitized(mut self) -> Self {
        self.version = CONFIG_VERSION;
        self.client_id = self.client_id.trim().to_owned();
        self.step = self.step.clamp(MIN_STEP, MAX_STEP);
        self.sync_delay_ms = self
            .sync_delay_ms
            .clamp(MIN_SYNC_DELAY_MS, MAX_SYNC_DELAY_MS);
        if self.redirect_port == 0 {
            self.redirect_port = Settings::default().redirect_port;
        }
        self.osd.duration_ms = self
            .osd
            .duration_ms
            .clamp(MIN_OSD_DURATION_MS, MAX_OSD_DURATION_MS);
        if !self.osd.margin.is_finite() || self.osd.margin < 0.0 {
            self.osd.margin = OsdSettings::default().margin;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bindings {
    pub volume_up: KeyCode,
    pub volume_down: KeyCode,
    pub mute: Option<KeyCode>,
    /// Swallow bound key presses so they never reach Windows or other apps.
    pub suppress: bool,
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            // F13 and F15: what the knob this was written for actually sends.
            // Function keys above F12 are what knobs and macro pads commonly
            // emit, since Windows has no other spare keys to give them.
            volume_up: KeyCode::Raw(0x7C),
            volume_down: KeyCode::Raw(0x7E),
            mute: None,
            suppress: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OsdPosition {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl OsdPosition {
    pub const ALL: [OsdPosition; 6] = [
        OsdPosition::TopLeft,
        OsdPosition::TopCenter,
        OsdPosition::TopRight,
        OsdPosition::BottomLeft,
        OsdPosition::BottomCenter,
        OsdPosition::BottomRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OsdPosition::TopLeft => "Top left",
            OsdPosition::TopCenter => "Top center",
            OsdPosition::TopRight => "Top right",
            OsdPosition::BottomLeft => "Bottom left",
            OsdPosition::BottomCenter => "Bottom center",
            OsdPosition::BottomRight => "Bottom right",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OsdSettings {
    pub enabled: bool,
    /// How long the popup stays visible after the last change.
    pub duration_ms: u64,
    pub position: OsdPosition,
    /// Distance from the screen edge(s), in logical points.
    pub margin: f32,
    /// Per-pixel transparent window. Needs a restart to change; falls back to
    /// an opaque dark flyout when `false` (for GPUs/drivers without alpha).
    pub transparent: bool,
}

impl Default for OsdSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_ms: 1500,
            position: OsdPosition::BottomCenter,
            margin: 48.0,
            transparent: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no per-user config directory is available on this system")]
    NoConfigDir,
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not valid: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("cannot serialize settings: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// `<user config dir>/knobify`, e.g. `C:\Users\me\AppData\Roaming\knobify`.
pub fn config_dir() -> Result<PathBuf, ConfigError> {
    dirs::config_dir()
        .map(|d| d.join(APP_DIR_NAME))
        .ok_or(ConfigError::NoConfigDir)
}

pub fn config_path() -> Result<PathBuf, ConfigError> {
    Ok(config_dir()?.join(CONFIG_FILE_NAME))
}

pub fn token_cache_path() -> Result<PathBuf, ConfigError> {
    Ok(config_dir()?.join(TOKEN_FILE_NAME))
}

/// Load settings; a missing file yields [`Settings::default`], a malformed
/// file is an error (the caller decides whether to fall back).
pub fn load(path: &Path) -> Result<Settings, ConfigError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_owned(),
                source,
            })
        }
    };
    let settings: Settings = toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.to_owned(),
        source,
    })?;
    Ok(settings.sanitized())
}

/// Atomically write settings (temp file + rename), creating parent directories.
///
/// Writes `settings` as given; callers pass [`Settings::sanitized`] values (the
/// UI sanitizes on Save, [`load`] sanitizes on read), so a config file written
/// by Knobify is always in range.
pub fn save(path: &Path, settings: &Settings) -> Result<(), ConfigError> {
    let text = toml::to_string_pretty(settings)?;
    let write_err = |source| ConfigError::Write {
        path: path.to_owned(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(write_err)?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(write_err)?;
    if let Err(source) = fs::rename(&tmp, path) {
        // Never leave a stale temporary file behind for the next launch.
        let _ = fs::remove_file(&tmp);
        return Err(write_err(source));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn default_serializes_and_parses_back_equal() {
        let settings = Settings::default();
        let text = toml::to_string_pretty(&settings).unwrap();
        let parsed: Settings = toml::from_str(&text).unwrap();
        assert_eq!(parsed, settings);
    }

    #[test]
    fn partial_toml_yields_defaults_elsewhere() {
        let parsed: Settings = toml::from_str("step = 10\n").unwrap();
        let expected = Settings {
            step: 10,
            ..Settings::default()
        };
        assert_eq!(parsed, expected);
    }

    #[test]
    fn an_older_file_without_a_sync_delay_gets_the_default() {
        let parsed: Settings = toml::from_str(
            "step = 5
",
        )
        .unwrap();
        assert_eq!(parsed.sync_delay_ms, DEFAULT_SYNC_DELAY_MS);
        assert_eq!(parsed.sync_delay(), Duration::from_millis(3000));
    }

    #[test]
    fn sanitized_clamps_the_sync_delay() {
        // Zero would believe every reading, including the ones that still
        // report the volume from before our own change.
        let low = Settings {
            sync_delay_ms: 0,
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(low.sync_delay_ms, MIN_SYNC_DELAY_MS);

        let high = Settings {
            sync_delay_ms: u64::MAX,
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(high.sync_delay_ms, MAX_SYNC_DELAY_MS);
    }

    #[test]
    fn sanitized_clamps_step() {
        let low = Settings {
            step: 0,
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(low.step, MIN_STEP);

        let high = Settings {
            step: 99,
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(high.step, MAX_STEP);
    }

    #[test]
    fn sanitized_clamps_osd_duration() {
        let low = Settings {
            osd: OsdSettings {
                duration_ms: 10,
                ..OsdSettings::default()
            },
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(low.osd.duration_ms, MIN_OSD_DURATION_MS);
        assert_eq!(MIN_OSD_DURATION_MS, 300);
    }

    #[test]
    fn load_missing_path_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let loaded = load(&path).unwrap();
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn save_then_load_round_trips_and_cleans_up_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let settings = Settings {
            step: 12,
            client_id: "abc123".to_owned(),
            ..Settings::default()
        };

        save(&path, &settings).unwrap();
        assert!(path.exists());
        assert!(!path.with_extension("toml.tmp").exists());

        let loaded = load(&path).unwrap();
        assert_eq!(loaded, settings.sanitized());
    }

    #[test]
    fn malformed_toml_yields_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "this is not valid toml = = =").unwrap();
        let err = load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }
}
