//! Chord parsing for the CLI: `ctrl+c`, `alt+enter`, `shift+f5`, `x`.
//! Reuses the spec vocabulary (`KeySpec`) so YAML and the command line agree.

use muse_core::error::{Error, Result};
use muse_core::input::{KeyEvent, MouseEvent};
use muse_runner::spec::{KeySpec, MouseSpec};

/// `mod+mod+key` → [`KeyEvent`]. The last `+`-separated token is the key; a
/// bare `+` is the plus key.
pub fn parse_chord(chord: &str) -> Result<KeyEvent> {
    if chord.is_empty() {
        return Err(Error::BadArgument("empty key chord".into()));
    }
    let mut parts: Vec<&str> = chord.split('+').collect();
    // "ctrl++" splits as ["ctrl", "", ""] → the key is '+'
    let key = match parts.pop() {
        Some("") if chord.ends_with('+') => {
            parts.pop();
            "+"
        }
        Some(k) => k,
        None => unreachable!(),
    };
    let mods = parts.iter().map(|m| m.to_string()).collect();
    KeySpec {
        key: key.to_lowercase(),
        mods,
    }
    .to_event()
}

/// `action:button@row,col` with optional `+mods` suffix on the button, e.g.
/// `press:left@3,10`, `release:left@3,10`, `press:wheel_down@0,0`,
/// `press:left+ctrl@1,1`. `action:` defaults to `press`, `button` to `left`,
/// so `@3,10` alone is a left click.
pub fn parse_mouse(spec: &str) -> Result<MouseEvent> {
    let (head, pos) = spec
        .split_once('@')
        .ok_or_else(|| Error::BadArgument(format!("mouse `{spec}`: expected …@row,col")))?;
    let (row, col) = pos
        .split_once(',')
        .ok_or_else(|| Error::BadArgument(format!("mouse `{spec}`: expected row,col")))?;
    let row: u16 = row
        .trim()
        .parse()
        .map_err(|_| Error::BadArgument(format!("mouse `{spec}`: bad row")))?;
    let col: u16 = col
        .trim()
        .parse()
        .map_err(|_| Error::BadArgument(format!("mouse `{spec}`: bad col")))?;
    let (action, button) = match head.split_once(':') {
        Some((a, b)) => (a, b),
        None if head.is_empty() => ("press", "left"),
        None => ("press", head),
    };
    let mut bparts = button.split('+');
    let button = bparts.next().unwrap_or("left");
    let mods: Vec<String> = bparts.map(|s| s.to_string()).collect();
    MouseSpec {
        row,
        col,
        button: if button.is_empty() {
            "left".into()
        } else {
            button.into()
        },
        action: if action.is_empty() {
            "press".into()
        } else {
            action.into()
        },
        mods,
    }
    .to_event()
}

/// `\x1b[A`, `\e[A`, `\n`, `\r`, `\t`, `\\` → bytes.
pub fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('e') => out.push(0x1b),
            Some('\\') => out.push(b'\\'),
            Some('0') => out.push(0),
            Some('x') => {
                let hi = chars.next();
                let lo = chars.next();
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        let hex = format!("{h}{l}");
                        match u8::from_str_radix(&hex, 16) {
                            Ok(b) => out.push(b),
                            Err(_) => out.extend_from_slice(format!("\\x{hex}").as_bytes()),
                        }
                    }
                    _ => out.extend_from_slice(b"\\x"),
                }
            }
            Some(other) => {
                out.push(b'\\');
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::input::{Key, Mods, MouseAction, MouseButton};

    #[test]
    fn chords() {
        assert_eq!(parse_chord("x").unwrap(), KeyEvent::new(Key::Char('x')));
        assert_eq!(
            parse_chord("ctrl+c").unwrap(),
            KeyEvent::with(Key::Char('c'), Mods::CTRL)
        );
        assert_eq!(
            parse_chord("Ctrl+Alt+Enter").unwrap(),
            KeyEvent::with(Key::Enter, Mods::CTRL | Mods::ALT)
        );
        assert_eq!(
            parse_chord("shift+F5").unwrap(),
            KeyEvent::with(Key::F(5), Mods::SHIFT)
        );
        assert_eq!(parse_chord("+").unwrap(), KeyEvent::new(Key::Char('+')));
        assert_eq!(
            parse_chord("ctrl++").unwrap(),
            KeyEvent::with(Key::Char('+'), Mods::CTRL)
        );
        assert!(parse_chord("").is_err());
        assert!(parse_chord("hyper+x").is_err());
        assert!(parse_chord("nosuchkey").is_err());
    }

    #[test]
    fn mouse_specs() {
        let ev = parse_mouse("@3,10").unwrap();
        assert_eq!((ev.row, ev.col), (3, 10));
        assert_eq!(ev.button, MouseButton::Left);
        assert_eq!(ev.action, MouseAction::Press);
        let ev = parse_mouse("release:right+ctrl@0,1").unwrap();
        assert_eq!(ev.button, MouseButton::Right);
        assert_eq!(ev.action, MouseAction::Release);
        assert!(ev.mods.contains(Mods::CTRL));
        let ev = parse_mouse("wheel_down@5,5").unwrap();
        assert_eq!(ev.button, MouseButton::WheelDown);
        assert!(parse_mouse("3,10").is_err());
        assert!(parse_mouse("@x,1").is_err());
        assert!(parse_mouse("click:left@1,1").is_err());
    }

    #[test]
    fn unescapes() {
        assert_eq!(unescape("\\x1b[A"), b"\x1b[A");
        assert_eq!(unescape("\\e[B\\n"), b"\x1b[B\n");
        assert_eq!(unescape("a\\\\b\\t\\r\\0"), b"a\\b\t\r\0");
        assert_eq!(unescape("\\q"), b"\\q");
        assert_eq!(unescape("\\xZZ"), b"\\xZZ");
        assert_eq!(unescape("é"), "é".as_bytes());
    }
}
