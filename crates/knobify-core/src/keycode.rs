//! Backend-independent identifier for a keyboard key.
//!
//! The binary crate converts between this and `rdev::Key`. Named keys use the
//! `rdev::Key` variant name (for example `VolumeUp`, `F13`, `KeyA`); keys rdev
//! does not know (like the OEM codes a hardware knob emits) are stored as the
//! raw Windows virtual-key code.
//!
//! Config form (serde / [`KeyCode::to_config_string`]): `"VolumeUp"` or `"0x82"`.
//! Human form ([`std::fmt::Display`]): `Volume Up` or `Key 0x82`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum KeyCode {
    /// A key rdev knows by name (`rdev::Key` variant name).
    Named(String),
    /// A raw virtual-key code (`rdev::Key::Unknown(code)`).
    Raw(u32),
}

impl KeyCode {
    pub fn named(name: impl Into<String>) -> Self {
        Self::Named(name.into())
    }

    pub fn raw(code: u32) -> Self {
        Self::Raw(code)
    }

    /// The form written to the config file and accepted by [`FromStr`].
    pub fn to_config_string(&self) -> String {
        match self {
            Self::Named(name) => name.clone(),
            Self::Raw(code) => format!("0x{code:02X}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyCodeParseError {
    #[error("key code is empty")]
    Empty,
    #[error("invalid hexadecimal key code `{0}`")]
    BadHex(String),
    #[error("invalid key name `{0}` (letters and digits only)")]
    BadName(String),
}

impl FromStr for KeyCode {
    type Err = KeyCodeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(KeyCodeParseError::Empty);
        }
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            return u32::from_str_radix(hex, 16)
                .map(Self::Raw)
                .map_err(|_| KeyCodeParseError::BadHex(s.to_owned()));
        }
        if s.chars().all(|c| c.is_ascii_alphanumeric()) {
            Ok(Self::Named(s.to_owned()))
        } else {
            Err(KeyCodeParseError::BadName(s.to_owned()))
        }
    }
}

impl TryFrom<String> for KeyCode {
    type Error = KeyCodeParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<KeyCode> for String {
    fn from(value: KeyCode) -> Self {
        value.to_config_string()
    }
}

impl fmt::Display for KeyCode {
    /// Human-readable label: `Volume Up`, `Key A`, `F13`, `Key 0x82`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw(code) => write!(f, "Key 0x{code:02X}"),
            Self::Named(name) => {
                let mut out = String::with_capacity(name.len() + 4);
                let mut prev_lower = false;
                for c in name.chars() {
                    if c.is_ascii_uppercase() && prev_lower {
                        out.push(' ');
                    }
                    prev_lower = c.is_ascii_lowercase();
                    out.push(c);
                }
                f.write_str(&out)
            }
        }
    }
}
