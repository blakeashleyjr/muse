//! Input encoding (§9): keys/mouse/paste → bytes, honoring mode state.

use crate::capabilities::{Capabilities, KeyboardProtocol};
use crate::modes::{ModeState, MouseEnc, MouseMode};
use serde::{Deserialize, Serialize};

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
    pub struct Mods: u8 {
        const SHIFT = 1;
        const ALT = 2;
        const CTRL = 4;
        const SUPER = 8;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
}

impl KeyEvent {
    pub fn new(key: Key) -> Self {
        KeyEvent {
            key,
            mods: Mods::empty(),
        }
    }

    pub fn with(key: Key, mods: Mods) -> Self {
        KeyEvent { key, mods }
    }
}

/// xterm modifier parameter: 1 + bitmask (shift=1, alt=2, ctrl=4).
fn xterm_mod_param(mods: Mods) -> u8 {
    let mut m = 0u8;
    if mods.contains(Mods::SHIFT) {
        m |= 1;
    }
    if mods.contains(Mods::ALT) {
        m |= 2;
    }
    if mods.contains(Mods::CTRL) {
        m |= 4;
    }
    if mods.contains(Mods::SUPER) {
        m |= 8;
    }
    m + 1
}

fn csi_with_mods(final_byte: u8, mods: Mods) -> Vec<u8> {
    // CSI 1 ; <mod> <final>
    let p = xterm_mod_param(mods);
    format!("\x1b[1;{}{}", p, final_byte as char).into_bytes()
}

/// Encode a key event to bytes, honoring negotiated modes & capabilities.
pub fn encode_key(ev: &KeyEvent, modes: &ModeState, caps: &Capabilities) -> Vec<u8> {
    let has_mods = !ev.mods.is_empty();
    let only_ctrl = ev.mods == Mods::CTRL;
    let only_alt = ev.mods == Mods::ALT;

    match ev.key {
        Key::Char(c) => encode_char(c, ev.mods, only_ctrl, only_alt, caps),
        Key::Enter => prefix_alt(b"\r".to_vec(), ev.mods),
        Key::Tab => {
            if ev.mods.contains(Mods::SHIFT) {
                b"\x1b[Z".to_vec()
            } else {
                prefix_alt(b"\t".to_vec(), ev.mods)
            }
        }
        Key::Backspace => prefix_alt(b"\x7f".to_vec(), ev.mods),
        Key::Escape => b"\x1b".to_vec(),
        Key::Up | Key::Down | Key::Right | Key::Left => {
            let fb = match ev.key {
                Key::Up => b'A',
                Key::Down => b'B',
                Key::Right => b'C',
                Key::Left => b'D',
                _ => unreachable!(),
            };
            if has_mods {
                csi_with_mods(fb, ev.mods)
            } else if modes.app_cursor_keys {
                vec![0x1b, b'O', fb]
            } else {
                vec![0x1b, b'[', fb]
            }
        }
        Key::Home | Key::End => {
            let fb = if ev.key == Key::Home { b'H' } else { b'F' };
            if has_mods {
                csi_with_mods(fb, ev.mods)
            } else {
                // ESC [ 1~ / 4~ legacy form
                let n = if ev.key == Key::Home { 1 } else { 4 };
                format!("\x1b[{}~", n).into_bytes()
            }
        }
        Key::PageUp | Key::PageDown | Key::Insert | Key::Delete => {
            let n = match ev.key {
                Key::Insert => 2,
                Key::Delete => 3,
                Key::PageUp => 5,
                Key::PageDown => 6,
                _ => unreachable!(),
            };
            if has_mods {
                format!("\x1b[{};{}~", n, xterm_mod_param(ev.mods)).into_bytes()
            } else {
                format!("\x1b[{}~", n).into_bytes()
            }
        }
        Key::F(n) => encode_fkey(n, ev.mods),
    }
}

fn encode_char(
    c: char,
    mods: Mods,
    only_ctrl: bool,
    only_alt: bool,
    _caps: &Capabilities,
) -> Vec<u8> {
    if only_ctrl {
        // Ctrl+Char = char & 0x1f for ASCII letters and a few symbols.
        let b = ctrl_byte(c);
        return vec![b];
    }
    if only_alt {
        // ESC + X
        let mut v = vec![0x1b];
        let mut buf = [0u8; 4];
        v.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        return v;
    }
    if mods.contains(Mods::CTRL) && mods.contains(Mods::ALT) {
        let mut v = vec![0x1b];
        v.push(ctrl_byte(c));
        return v;
    }
    // SHIFT (or no mods) — emit the character as-is (caller supplies shifted char).
    let mut buf = [0u8; 4];
    c.encode_utf8(&mut buf).as_bytes().to_vec()
}

fn ctrl_byte(c: char) -> u8 {
    let up = c.to_ascii_uppercase();
    match up {
        '@'..='_' => (up as u8) & 0x1f,
        'a'..='z' => (up as u8) & 0x1f,
        ' ' => 0,
        '?' => 0x7f,
        _ => (c as u8) & 0x1f,
    }
}

fn prefix_alt(mut bytes: Vec<u8>, mods: Mods) -> Vec<u8> {
    if mods.contains(Mods::ALT) {
        let mut v = vec![0x1b];
        v.append(&mut bytes);
        v
    } else {
        bytes
    }
}

fn encode_fkey(n: u8, mods: Mods) -> Vec<u8> {
    // F1-F4 = ESC O P/Q/R/S ; F5-F12 = ESC [ 15~..24~
    let base = match n {
        1 => return fkey_pqrs(b'P', mods),
        2 => return fkey_pqrs(b'Q', mods),
        3 => return fkey_pqrs(b'R', mods),
        4 => return fkey_pqrs(b'S', mods),
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return vec![],
    };
    if mods.is_empty() {
        format!("\x1b[{}~", base).into_bytes()
    } else {
        format!("\x1b[{};{}~", base, xterm_mod_param(mods)).into_bytes()
    }
}

fn fkey_pqrs(letter: u8, mods: Mods) -> Vec<u8> {
    if mods.is_empty() {
        vec![0x1b, b'O', letter]
    } else {
        format!("\x1b[1;{}{}", xterm_mod_param(mods), letter as char).into_bytes()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseAction {
    Press,
    Release,
    Move,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MouseEvent {
    pub button: MouseButton,
    pub action: MouseAction,
    /// 0-indexed grid coordinates.
    pub row: u16,
    pub col: u16,
    pub mods: Mods,
}

/// Encode a mouse event. Returns empty when mouse reporting is disabled.
pub fn encode_mouse(ev: &MouseEvent, modes: &ModeState) -> Vec<u8> {
    if modes.mouse == MouseMode::Off {
        return vec![];
    }
    if ev.action == MouseAction::Move
        && !matches!(modes.mouse, MouseMode::ButtonEvent | MouseMode::AnyEvent)
    {
        return vec![];
    }
    let mut cb: u32 = match ev.button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if ev.action == MouseAction::Move {
        cb += 32;
    }
    if ev.mods.contains(Mods::SHIFT) {
        cb += 4;
    }
    if ev.mods.contains(Mods::ALT) {
        cb += 8;
    }
    if ev.mods.contains(Mods::CTRL) {
        cb += 16;
    }
    let col = ev.col + 1;
    let row = ev.row + 1;
    match modes.mouse_encoding {
        MouseEnc::Sgr => {
            let final_byte = if ev.action == MouseAction::Release {
                'm'
            } else {
                'M'
            };
            format!("\x1b[<{};{};{}{}", cb, col, row, final_byte).into_bytes()
        }
        _ => {
            // Default X10 encoding: ESC [ M Cb Cx Cy (each +32)
            let mut v = vec![0x1b, b'[', b'M'];
            let cbn = if ev.action == MouseAction::Release {
                3
            } else {
                cb
            };
            v.push((cbn as u8).wrapping_add(32));
            v.push((col as u8).wrapping_add(32));
            v.push((row as u8).wrapping_add(32));
            v
        }
    }
}

/// Encode a paste, honoring bracketed-paste mode.
pub fn encode_paste(data: &[u8], modes: &ModeState) -> Vec<u8> {
    if modes.bracketed_paste {
        let mut v = b"\x1b[200~".to_vec();
        v.extend_from_slice(data);
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        data.to_vec()
    }
}

/// Whether the Kitty keyboard protocol should be used for `caps`/`modes`.
pub fn kitty_active(caps: &Capabilities, modes: &ModeState) -> bool {
    caps.keyboard == KeyboardProtocol::Kitty && modes.kitty_kbd_flags != 0
}

#[cfg(test)]
mod tests {
    // Several tests toggle a single mode field then re-encode; struct-literal
    // form would not express the mutate-and-retest pattern.
    #![allow(clippy::field_reassign_with_default)]
    use super::*;

    fn caps() -> Capabilities {
        Capabilities::default()
    }

    #[test]
    fn arrows_app_vs_normal() {
        let mut m = ModeState::default();
        m.app_cursor_keys = true;
        assert_eq!(
            encode_key(&KeyEvent::new(Key::Up), &m, &caps()),
            b"\x1bOA".to_vec()
        );
        m.app_cursor_keys = false;
        assert_eq!(
            encode_key(&KeyEvent::new(Key::Up), &m, &caps()),
            b"\x1b[A".to_vec()
        );
    }

    #[test]
    fn all_arrows_normal() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(&KeyEvent::new(Key::Down), &m, &caps()),
            b"\x1b[B"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::Right), &m, &caps()),
            b"\x1b[C"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::Left), &m, &caps()),
            b"\x1b[D"
        );
    }

    #[test]
    fn ctrl_c() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(&KeyEvent::with(Key::Char('c'), Mods::CTRL), &m, &caps()),
            vec![0x03]
        );
    }

    #[test]
    fn ctrl_space_and_question() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(&KeyEvent::with(Key::Char(' '), Mods::CTRL), &m, &caps()),
            vec![0]
        );
        assert_eq!(
            encode_key(&KeyEvent::with(Key::Char('?'), Mods::CTRL), &m, &caps()),
            vec![0x7f]
        );
    }

    #[test]
    fn alt_x() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(&KeyEvent::with(Key::Char('x'), Mods::ALT), &m, &caps()),
            vec![0x1b, b'x']
        );
    }

    #[test]
    fn ctrl_alt_combo() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(
                &KeyEvent::with(Key::Char('a'), Mods::CTRL | Mods::ALT),
                &m,
                &caps()
            ),
            vec![0x1b, 0x01]
        );
    }

    #[test]
    fn plain_char() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(&KeyEvent::new(Key::Char('A')), &m, &caps()),
            b"A"
        );
    }

    #[test]
    fn enter_tab_backspace_escape() {
        let m = ModeState::default();
        assert_eq!(encode_key(&KeyEvent::new(Key::Enter), &m, &caps()), b"\r");
        assert_eq!(encode_key(&KeyEvent::new(Key::Tab), &m, &caps()), b"\t");
        assert_eq!(
            encode_key(&KeyEvent::new(Key::Backspace), &m, &caps()),
            b"\x7f"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::Escape), &m, &caps()),
            b"\x1b"
        );
    }

    #[test]
    fn shift_tab() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(&KeyEvent::with(Key::Tab, Mods::SHIFT), &m, &caps()),
            b"\x1b[Z"
        );
    }

    #[test]
    fn alt_enter() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(&KeyEvent::with(Key::Enter, Mods::ALT), &m, &caps()),
            vec![0x1b, b'\r']
        );
    }

    #[test]
    fn fkeys() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(&KeyEvent::new(Key::F(1)), &m, &caps()),
            b"\x1bOP"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::F(4)), &m, &caps()),
            b"\x1bOS"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::F(5)), &m, &caps()),
            b"\x1b[15~"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::F(12)), &m, &caps()),
            b"\x1b[24~"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::F(99)), &m, &caps()),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn fkey_with_mods() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(&KeyEvent::with(Key::F(1), Mods::SHIFT), &m, &caps()),
            b"\x1b[1;2P"
        );
        assert_eq!(
            encode_key(&KeyEvent::with(Key::F(5), Mods::CTRL), &m, &caps()),
            b"\x1b[15;5~"
        );
    }

    #[test]
    fn nav_keys() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(&KeyEvent::new(Key::Home), &m, &caps()),
            b"\x1b[1~"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::End), &m, &caps()),
            b"\x1b[4~"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::PageUp), &m, &caps()),
            b"\x1b[5~"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::PageDown), &m, &caps()),
            b"\x1b[6~"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::Insert), &m, &caps()),
            b"\x1b[2~"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(Key::Delete), &m, &caps()),
            b"\x1b[3~"
        );
    }

    #[test]
    fn nav_keys_with_mods() {
        let m = ModeState::default();
        assert_eq!(
            encode_key(&KeyEvent::with(Key::Home, Mods::CTRL), &m, &caps()),
            b"\x1b[1;5H"
        );
        assert_eq!(
            encode_key(&KeyEvent::with(Key::PageUp, Mods::SHIFT), &m, &caps()),
            b"\x1b[5;2~"
        );
        assert_eq!(
            encode_key(&KeyEvent::with(Key::Up, Mods::CTRL), &m, &caps()),
            b"\x1b[1;5A"
        );
    }

    #[test]
    fn mouse_disabled_noop() {
        let m = ModeState::default();
        let ev = MouseEvent {
            button: MouseButton::Left,
            action: MouseAction::Press,
            row: 2,
            col: 4,
            mods: Mods::empty(),
        };
        assert!(encode_mouse(&ev, &m).is_empty());
    }

    #[test]
    fn mouse_sgr_click() {
        let mut m = ModeState::default();
        m.mouse = MouseMode::Normal;
        m.mouse_encoding = MouseEnc::Sgr;
        let ev = MouseEvent {
            button: MouseButton::Left,
            action: MouseAction::Press,
            row: 2,
            col: 4,
            mods: Mods::empty(),
        };
        // row3/col5 (1-indexed) => ESC[<0;5;3M
        assert_eq!(encode_mouse(&ev, &m), b"\x1b[<0;5;3M");
    }

    #[test]
    fn mouse_sgr_release() {
        let mut m = ModeState::default();
        m.mouse = MouseMode::Normal;
        m.mouse_encoding = MouseEnc::Sgr;
        let ev = MouseEvent {
            button: MouseButton::Left,
            action: MouseAction::Release,
            row: 0,
            col: 0,
            mods: Mods::empty(),
        };
        assert_eq!(encode_mouse(&ev, &m), b"\x1b[<0;1;1m");
    }

    #[test]
    fn mouse_move_requires_motion_mode() {
        let mut m = ModeState::default();
        m.mouse = MouseMode::Normal;
        m.mouse_encoding = MouseEnc::Sgr;
        let ev = MouseEvent {
            button: MouseButton::Left,
            action: MouseAction::Move,
            row: 0,
            col: 0,
            mods: Mods::empty(),
        };
        assert!(encode_mouse(&ev, &m).is_empty());
        m.mouse = MouseMode::AnyEvent;
        assert_eq!(encode_mouse(&ev, &m), b"\x1b[<32;1;1M");
    }

    #[test]
    fn mouse_default_encoding_x10() {
        let mut m = ModeState::default();
        m.mouse = MouseMode::Normal;
        m.mouse_encoding = MouseEnc::Default;
        let ev = MouseEvent {
            button: MouseButton::Left,
            action: MouseAction::Press,
            row: 0,
            col: 0,
            mods: Mods::empty(),
        };
        assert_eq!(encode_mouse(&ev, &m), vec![0x1b, b'[', b'M', 32, 33, 33]);
    }

    #[test]
    fn mouse_modifiers_and_wheel() {
        let mut m = ModeState::default();
        m.mouse = MouseMode::Normal;
        m.mouse_encoding = MouseEnc::Sgr;
        let ev = MouseEvent {
            button: MouseButton::WheelUp,
            action: MouseAction::Press,
            row: 0,
            col: 0,
            mods: Mods::CTRL | Mods::SHIFT | Mods::ALT,
        };
        // 64 + 4 + 8 + 16 = 92
        assert_eq!(encode_mouse(&ev, &m), b"\x1b[<92;1;1M");
    }

    #[test]
    fn paste_bracketed_and_raw() {
        let mut m = ModeState::default();
        assert_eq!(encode_paste(b"hi", &m), b"hi");
        m.bracketed_paste = true;
        assert_eq!(encode_paste(b"hi", &m), b"\x1b[200~hi\x1b[201~");
    }

    #[test]
    fn kitty_active_check() {
        let mut caps = caps();
        let mut m = ModeState::default();
        assert!(!kitty_active(&caps, &m));
        caps.keyboard = KeyboardProtocol::Kitty;
        assert!(!kitty_active(&caps, &m));
        m.kitty_kbd_flags = 1;
        assert!(kitty_active(&caps, &m));
    }

    #[test]
    fn mods_param_super() {
        assert_eq!(xterm_mod_param(Mods::SUPER), 9);
    }
}
