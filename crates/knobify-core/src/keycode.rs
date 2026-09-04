//! Backend-independent identifier for a keyboard key.
//!
//! A key is identified by its Windows virtual-key code. [`KeyCode::Raw`] is the
//! canonical form the keyboard hook produces; [`KeyCode::Named`] exists so that
//! a hand-written or older config can say `KeyA` or `VolumeUp` instead of a hex
//! number, and so that both spellings still match the same physical key.
//!
//! Config form (serde / [`KeyCode::to_config_string`]): `"VolumeUp"` or `"0x82"`.
//! Human form ([`std::fmt::Display`]): `Volume Up`, `F13`, `Key 0x82`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Virtual-key codes that carry a name, beyond the letters, digits and function
/// keys handled arithmetically. The names match the ones earlier versions wrote
/// into config files, so those keep working.
const NAMED_VKS: &[(u32, &str)] = &[
    (0x08, "Backspace"),
    (0x09, "Tab"),
    (0x0D, "Return"),
    (0x13, "Pause"),
    (0x14, "CapsLock"),
    (0x1B, "Escape"),
    (0x20, "Space"),
    (0x21, "PageUp"),
    (0x22, "PageDown"),
    (0x23, "End"),
    (0x24, "Home"),
    (0x25, "LeftArrow"),
    (0x26, "UpArrow"),
    (0x27, "RightArrow"),
    (0x28, "DownArrow"),
    (0x2C, "PrintScreen"),
    (0x2D, "Insert"),
    (0x2E, "Delete"),
    (0x5B, "MetaLeft"),
    (0x5C, "MetaRight"),
    (0x90, "NumLock"),
    (0x91, "ScrollLock"),
    (0xA0, "ShiftLeft"),
    (0xA1, "ShiftRight"),
    (0xA2, "ControlLeft"),
    (0xA3, "ControlRight"),
    (0xA4, "Alt"),
    (0xA5, "AltGr"),
    (0xAD, "VolumeMute"),
    (0xAE, "VolumeDown"),
    (0xAF, "VolumeUp"),
    (0xB0, "MediaNextTrack"),
    (0xB1, "MediaPrevTrack"),
    (0xB2, "MediaStop"),
    (0xB3, "MediaPlayPause"),
];

/// The name for a virtual-key code, if it has one.
fn name_for_vk(vk: u32) -> Option<String> {
    match vk {
        0x30..=0x39 => Some(format!("Num{}", vk - 0x30)),
        0x41..=0x5A => Some(format!("Key{}", (vk as u8) as char)),
        // F1 is 0x70 and the function keys run consecutively to F24.
        0x70..=0x87 => Some(format!("F{}", vk - 0x6F)),
        _ => NAMED_VKS
            .iter()
            .find(|(code, _)| *code == vk)
            .map(|(_, name)| (*name).to_owned()),
    }
}

/// The virtual-key code a name refers to, if it is one we know.
fn vk_for_name(name: &str) -> Option<u32> {
    if let Some(digit) = name.strip_prefix("Num") {
        if let Ok(d) = digit.parse::<u32>() {
            if d <= 9 {
                return Some(0x30 + d);
            }
        }
    }
    if let Some(letter) = name.strip_prefix("Key") {
        let mut chars = letter.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_uppercase() {
                return Some(c as u32);
            }
        }
    }
    if let Some(number) = name.strip_prefix('F') {
        if let Ok(n) = number.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x6F + n);
            }
        }
    }
    NAMED_VKS
        .iter()
        .find(|(_, known)| *known == name)
        .map(|(vk, _)| *vk)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum KeyCode {
    /// A key referred to by name, e.g. `KeyA`, `F13`, `VolumeUp`.
    Named(String),
    /// A raw Windows virtual-key code. What the hook produces.
    Raw(u32),
}

impl KeyCode {
    pub fn named(name: impl Into<String>) -> Self {
        Self::Named(name.into())
    }

    pub fn raw(code: u32) -> Self {
        Self::Raw(code)
    }

    /// The virtual-key code this refers to, or `None` for an unknown name.
    ///
    /// Comparing keys through this makes `"KeyA"` and `"0x41"` the same binding.
    pub fn vk(&self) -> Option<u32> {
        match self {
            Self::Raw(code) => Some(*code),
            Self::Named(name) => vk_for_name(name),
        }
    }

    /// Build the canonical form for a code the keyboard hook reported.
    pub fn from_vk(vk: u32) -> Self {
        Self::Raw(vk)
    }

    /// Do these refer to the same physical key, whichever way each is spelled?
    pub fn same_key(&self, other: &Self) -> bool {
        match (self.vk(), other.vk()) {
            (Some(a), Some(b)) => a == b,
            // Unknown names can still match each other verbatim.
            _ => self == other,
        }
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
    ///
    /// A raw code is shown by name when it has one, so the F13 a knob sends
    /// reads as `F13` rather than `Key 0x7C`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw(code) => match name_for_vk(*code) {
                Some(name) => fmt::Display::fmt(&Self::Named(name), f),
                None => write!(f, "Key 0x{code:02X}"),
            },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_lowercase_and_uppercase_prefix() {
        assert_eq!("0x82".parse::<KeyCode>().unwrap(), KeyCode::Raw(0x82));
        assert_eq!("0X82".parse::<KeyCode>().unwrap(), KeyCode::Raw(0x82));
    }

    #[test]
    fn parses_named_key() {
        assert_eq!(
            "VolumeUp".parse::<KeyCode>().unwrap(),
            KeyCode::Named("VolumeUp".to_owned())
        );
    }

    #[test]
    fn rejects_empty_string() {
        assert_eq!("".parse::<KeyCode>(), Err(KeyCodeParseError::Empty));
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(matches!(
            "0xZZ".parse::<KeyCode>(),
            Err(KeyCodeParseError::BadHex(_))
        ));
    }

    #[test]
    fn rejects_name_with_space() {
        assert!(matches!(
            "Volume Up".parse::<KeyCode>(),
            Err(KeyCodeParseError::BadName(_))
        ));
    }

    #[test]
    fn to_config_string_round_trips_through_from_str() {
        for original in [KeyCode::Raw(0x82), KeyCode::named("VolumeUp")] {
            let text = original.to_config_string();
            let parsed: KeyCode = text.parse().unwrap();
            assert_eq!(parsed, original);
        }
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Wrapper {
        key: KeyCode,
    }

    #[test]
    fn serde_round_trips_inside_a_struct_via_toml() {
        for key in [KeyCode::Raw(0x82), KeyCode::named("VolumeUp")] {
            let wrapper = Wrapper { key };
            let text = toml::to_string(&wrapper).unwrap();
            let parsed: Wrapper = toml::from_str(&text).unwrap();
            assert_eq!(parsed, wrapper);
        }
    }

    #[test]
    fn display_formats_human_readable_labels() {
        assert_eq!(KeyCode::named("VolumeUp").to_string(), "Volume Up");
        assert_eq!(KeyCode::named("F13").to_string(), "F13");
        assert_eq!(KeyCode::named("KeyA").to_string(), "Key A");
        // A raw code Windows names is shown by that name...
        assert_eq!(KeyCode::Raw(0x7C).to_string(), "F13");
        assert_eq!(KeyCode::Raw(0x82).to_string(), "F19");
        assert_eq!(KeyCode::Raw(0xAF).to_string(), "Volume Up");
        // ... and one it does not, by number.
        assert_eq!(KeyCode::Raw(0x07).to_string(), "Key 0x07");
    }

    #[test]
    fn names_and_numbers_resolve_to_the_same_key() {
        assert!(KeyCode::named("F13").same_key(&KeyCode::Raw(0x7C)));
        assert!(KeyCode::named("KeyA").same_key(&KeyCode::Raw(0x41)));
        assert!(KeyCode::named("Num0").same_key(&KeyCode::Raw(0x30)));
        assert!(KeyCode::named("VolumeMute").same_key(&KeyCode::Raw(0xAD)));
        assert!(!KeyCode::named("F13").same_key(&KeyCode::Raw(0x7D)));
    }

    #[test]
    fn vk_lookup_covers_every_documented_name() {
        for (vk, name) in NAMED_VKS {
            assert_eq!(vk_for_name(name), Some(*vk), "{name}");
            assert_eq!(name_for_vk(*vk).as_deref(), Some(*name), "0x{vk:02X}");
        }
        // The arithmetic ranges round-trip too.
        for vk in [0x30u32, 0x39, 0x41, 0x5A, 0x70, 0x87] {
            let name = name_for_vk(vk).expect("named range");
            assert_eq!(vk_for_name(&name), Some(vk), "{name}");
        }
        // Out of range must not be invented.
        assert_eq!(vk_for_name("F25"), None);
        assert_eq!(vk_for_name("F0"), None);
        assert_eq!(vk_for_name("KeyAB"), None);
        assert_eq!(vk_for_name("Num10"), None);
        assert_eq!(vk_for_name("Nonsense"), None);
    }

    #[test]
    fn an_unknown_name_still_matches_itself_but_nothing_else() {
        let mystery = KeyCode::named("Nonsense");
        assert_eq!(mystery.vk(), None);
        assert!(mystery.same_key(&KeyCode::named("Nonsense")));
        assert!(!mystery.same_key(&KeyCode::Raw(0x7C)));
    }
}
