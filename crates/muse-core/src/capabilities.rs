//! Emulation capability tables and profiles (pure data; see §6).
//!
//! These live in core because input encoding reads them and core cannot depend
//! on `muse-emulator`. The emulator crate provides the concrete built-in
//! profiles and the backend.

use crate::modes::MouseMode;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorDepth {
    NoColor,
    Ansi16,
    Indexed256,
    TrueColor,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidthMode {
    EastAsianAmbiguousNarrow,
    Wide,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardProtocol {
    Legacy,
    ModifyOtherKeys,
    Kitty,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub terminfo_name: String,
    pub color: ColorDepth,
    pub width_mode: WidthMode,
    pub keyboard: KeyboardProtocol,
    pub mouse: Vec<MouseMode>,
    pub supports_sync_output: bool,
    pub supports_bracketed_paste: bool,
    pub tab_width: u8,
    pub da1: Vec<u8>,
    pub da2: Vec<u8>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            terminfo_name: "xterm-256color".into(),
            color: ColorDepth::TrueColor,
            width_mode: WidthMode::Wide,
            keyboard: KeyboardProtocol::ModifyOtherKeys,
            mouse: vec![
                MouseMode::X10,
                MouseMode::Normal,
                MouseMode::ButtonEvent,
                MouseMode::AnyEvent,
            ],
            supports_sync_output: true,
            supports_bracketed_paste: true,
            tab_width: 8,
            da1: b"\x1b[?64;1;2;6;9;15;18;21;22c".to_vec(),
            da2: b"\x1b[>0;276;0c".to_vec(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub caps: Capabilities,
    pub env: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_depth_ordering() {
        assert!(ColorDepth::NoColor < ColorDepth::TrueColor);
        assert!(ColorDepth::Ansi16 < ColorDepth::Indexed256);
    }

    #[test]
    fn default_caps() {
        let c = Capabilities::default();
        assert_eq!(c.terminfo_name, "xterm-256color");
        assert_eq!(c.color, ColorDepth::TrueColor);
        assert!(c.supports_bracketed_paste);
        assert_eq!(c.tab_width, 8);
    }

    #[test]
    fn serde_roundtrip() {
        let p = Profile {
            name: "x".into(),
            caps: Capabilities::default(),
            env: vec![("A".into(), "B".into())],
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Profile>(&s).unwrap(), p);
    }
}
