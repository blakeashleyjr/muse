//! Built-in emulation profiles (§6).

use muse_core::capabilities::{Capabilities, ColorDepth, KeyboardProtocol, WidthMode};
use muse_core::modes::MouseMode;
use muse_core::Profile;

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub fn xterm() -> Profile {
    Profile {
        name: "xterm".into(),
        caps: Capabilities {
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
        },
        env: env(&[("COLORTERM", "truecolor")]),
    }
}

pub fn vt220() -> Profile {
    Profile {
        name: "vt220".into(),
        caps: Capabilities {
            terminfo_name: "vt220".into(),
            color: ColorDepth::Ansi16,
            width_mode: WidthMode::EastAsianAmbiguousNarrow,
            keyboard: KeyboardProtocol::Legacy,
            mouse: vec![],
            supports_sync_output: false,
            supports_bracketed_paste: false,
            tab_width: 8,
            da1: b"\x1b[?62;1;2;6;8;9c".to_vec(),
            da2: b"\x1b[>1;10;0c".to_vec(),
        },
        env: env(&[]),
    }
}

pub fn kitty() -> Profile {
    Profile {
        name: "kitty".into(),
        caps: Capabilities {
            terminfo_name: "xterm-kitty".into(),
            color: ColorDepth::TrueColor,
            width_mode: WidthMode::Wide,
            keyboard: KeyboardProtocol::Kitty,
            mouse: vec![
                MouseMode::X10,
                MouseMode::Normal,
                MouseMode::ButtonEvent,
                MouseMode::AnyEvent,
            ],
            supports_sync_output: true,
            supports_bracketed_paste: true,
            tab_width: 8,
            da1: b"\x1b[?62;c".to_vec(),
            da2: b"\x1b[>1;4000;0c".to_vec(),
        },
        env: env(&[("COLORTERM", "truecolor"), ("TERM_PROGRAM", "kitty")]),
    }
}

pub fn screen() -> Profile {
    Profile {
        name: "screen".into(),
        caps: Capabilities {
            terminfo_name: "screen".into(),
            color: ColorDepth::Indexed256,
            width_mode: WidthMode::Wide,
            keyboard: KeyboardProtocol::Legacy,
            mouse: vec![MouseMode::Normal],
            supports_sync_output: false,
            supports_bracketed_paste: true,
            tab_width: 8,
            da1: b"\x1b[?1;2c".to_vec(),
            da2: b"\x1b[>83;40500;0c".to_vec(),
        },
        env: env(&[]),
    }
}

pub fn dumb() -> Profile {
    Profile {
        name: "dumb".into(),
        caps: Capabilities {
            terminfo_name: "dumb".into(),
            color: ColorDepth::NoColor,
            width_mode: WidthMode::EastAsianAmbiguousNarrow,
            keyboard: KeyboardProtocol::Legacy,
            mouse: vec![],
            supports_sync_output: false,
            supports_bracketed_paste: false,
            tab_width: 8,
            da1: b"\x1b[?1c".to_vec(),
            da2: b"\x1b[>0;0;0c".to_vec(),
        },
        env: env(&[]),
    }
}

/// Look up a built-in profile by name.
pub fn by_name(name: &str) -> Option<Profile> {
    Some(match name {
        "xterm" => xterm(),
        "vt220" => vt220(),
        "kitty" => kitty(),
        "screen" => screen(),
        "dumb" => dumb(),
        _ => return None,
    })
}

/// All built-in profile names.
pub fn all_names() -> &'static [&'static str] {
    &["xterm", "vt220", "kitty", "screen", "dumb"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_each() {
        for name in all_names() {
            let p = by_name(name).unwrap();
            assert_eq!(p.name, *name);
        }
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn xterm_is_truecolor() {
        assert_eq!(xterm().caps.color, ColorDepth::TrueColor);
    }

    #[test]
    fn vt220_no_mouse_no_paste() {
        let p = vt220();
        assert!(p.caps.mouse.is_empty());
        assert!(!p.caps.supports_bracketed_paste);
        assert_eq!(p.caps.da1, b"\x1b[?62;1;2;6;8;9c");
    }

    #[test]
    fn dumb_no_color() {
        assert_eq!(dumb().caps.color, ColorDepth::NoColor);
    }

    #[test]
    fn kitty_keyboard() {
        assert_eq!(kitty().caps.keyboard, KeyboardProtocol::Kitty);
    }

    #[test]
    fn screen_256() {
        assert_eq!(screen().caps.color, ColorDepth::Indexed256);
        assert!(!screen().caps.supports_sync_output);
    }
}
