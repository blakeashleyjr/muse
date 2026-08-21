//! Terminal mode state mirrored from the SUT. Input encoders read this.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseMode {
    #[default]
    Off,
    /// X10 compatibility (press only).
    X10,
    /// Normal tracking (press + release).
    Normal,
    /// Button-event tracking (motion while a button is held).
    ButtonEvent,
    /// Any-event tracking (all motion).
    AnyEvent,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseEnc {
    #[default]
    Default,
    Utf8,
    /// SGR 1006 extended encoding.
    Sgr,
    Urxvt,
}

/// Mirror of the SUT's negotiated mode state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeState {
    /// DECCKM — application cursor keys.
    pub app_cursor_keys: bool,
    pub app_keypad: bool,
    /// DEC 2004 — bracketed paste.
    pub bracketed_paste: bool,
    pub mouse: MouseMode,
    pub mouse_encoding: MouseEnc,
    /// DEC 2026 — synchronized output in progress.
    pub sync_output: bool,
    /// Kitty keyboard protocol flags currently in effect (top of the push
    /// stack); 0 = legacy encoding.
    pub kitty_kbd_flags: u8,
    /// xterm `modifyOtherKeys` level (`CSI > 4 ; n m`); 0 = off.
    pub modify_other_keys: u8,
    pub alt_screen: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let m = ModeState::default();
        assert!(!m.app_cursor_keys);
        assert_eq!(m.mouse, MouseMode::Off);
        assert_eq!(m.mouse_encoding, MouseEnc::Default);
        assert!(!m.bracketed_paste);
        assert!(!m.alt_screen);
    }

    #[test]
    fn serde_roundtrip() {
        let m = ModeState {
            app_cursor_keys: true,
            bracketed_paste: true,
            mouse: MouseMode::AnyEvent,
            mouse_encoding: MouseEnc::Sgr,
            ..Default::default()
        };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<ModeState>(&s).unwrap(), m);
    }
}
