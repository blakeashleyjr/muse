//! Turn what was done to a session into a runnable `muse run` spec: the
//! inputs become `write`/`key`/`paste`/`mouse`/`resize` steps and every
//! `wait` that held becomes the matching `expect_*` step. This is how an
//! agent promotes an interactive check into a regression test.

use super::proto::{Input, WaitCond};
use muse_core::input::{Key, KeyEvent, Mods, MouseAction, MouseButton, MouseEvent};
use muse_core::locator::Locator;

/// One thing that happened to a session, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Recorded {
    Input(Input),
    Resize(u16, u16),
    /// A wait and whether it held. Failed waits are kept for context but
    /// rendered commented-out.
    Wait {
        cond: WaitCond,
        ok: bool,
        timeout_ms: u64,
    },
}

/// What the spec header needs.
pub struct SpecHeader<'a> {
    pub name: &'a str,
    pub argv: &'a [String],
    pub profile: &'a str,
    pub cols: u16,
    pub rows: u16,
    pub env: &'a [(String, String)],
}

/// A YAML double-quoted scalar (JSON string syntax is valid YAML).
fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}

fn key_name(k: Key) -> String {
    match k {
        Key::Char(c) => c.to_string(),
        Key::Enter => "enter".into(),
        Key::Tab => "tab".into(),
        Key::Backspace => "backspace".into(),
        Key::Escape => "escape".into(),
        Key::Up => "up".into(),
        Key::Down => "down".into(),
        Key::Left => "left".into(),
        Key::Right => "right".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::PageUp => "pageup".into(),
        Key::PageDown => "pagedown".into(),
        Key::Insert => "insert".into(),
        Key::Delete => "delete".into(),
        Key::F(n) => format!("f{n}"),
    }
}

fn mods_list(m: Mods) -> String {
    let mut v = Vec::new();
    if m.contains(Mods::CTRL) {
        v.push("ctrl");
    }
    if m.contains(Mods::ALT) {
        v.push("alt");
    }
    if m.contains(Mods::SHIFT) {
        v.push("shift");
    }
    if m.contains(Mods::SUPER) {
        v.push("super");
    }
    format!("[{}]", v.join(", "))
}

fn key_step(ev: &KeyEvent) -> String {
    if ev.mods.is_empty() {
        format!("key: {{key: {}}}", q(&key_name(ev.key)))
    } else {
        format!(
            "key: {{key: {}, mods: {}}}",
            q(&key_name(ev.key)),
            mods_list(ev.mods)
        )
    }
}

fn mouse_step(ev: &MouseEvent) -> String {
    let button = match ev.button {
        MouseButton::Left => "left",
        MouseButton::Middle => "middle",
        MouseButton::Right => "right",
        MouseButton::WheelUp => "wheel_up",
        MouseButton::WheelDown => "wheel_down",
    };
    let action = match ev.action {
        MouseAction::Press => "press",
        MouseAction::Release => "release",
        MouseAction::Move => "move",
    };
    let mut s = format!(
        "mouse: {{row: {}, col: {}, button: {button}, action: {action}",
        ev.row, ev.col
    );
    if !ev.mods.is_empty() {
        s.push_str(&format!(", mods: {}", mods_list(ev.mods)));
    }
    s.push('}');
    s
}

/// Locator fields as `k: v, …` (no braces).
fn loc_fields(loc: &Locator) -> Option<String> {
    Some(match loc {
        Locator::Text {
            pattern,
            ignore_case,
            whole_line,
        } => {
            let mut s = format!("text: {}", q(pattern));
            if *ignore_case {
                s.push_str(", ignore_case: true");
            }
            if *whole_line {
                s.push_str(", whole_line: true");
            }
            s
        }
        Locator::Regex { re } => format!("regex: {}", q(re)),
        Locator::Line { row } => format!("line: {row}"),
        Locator::Cell { row, col } => format!("cell: [{row}, {col}]"),
        Locator::Region { rect } => format!(
            "region: [{}, {}, {}, {}]",
            rect.row, rect.col, rect.w, rect.h
        ),
        Locator::Cursor => "cursor: true".into(),
        _ => return None,
    })
}

fn wait_step(cond: &WaitCond, timeout_ms: u64) -> Option<String> {
    let t = format!(", timeout_ms: {timeout_ms}");
    Some(match cond {
        WaitCond::Visible { loc, multiline } => format!(
            "expect_visible: {{{}{}{t}}}",
            loc_fields(loc)?,
            if *multiline { ", multiline: true" } else { "" }
        ),
        WaitCond::NotVisible { loc, .. } => {
            format!("expect_not_visible: {{{}{t}}}", loc_fields(loc)?)
        }
        WaitCond::Text { loc, equals, .. } => format!(
            "expect_text: {{{}, equals: {}{t}}}",
            loc_fields(loc)?,
            q(equals)
        ),
        WaitCond::Contains { loc, contains, .. } => format!(
            "expect_contains: {{{}, contains: {}{t}}}",
            loc_fields(loc)?,
            q(contains)
        ),
        WaitCond::Count {
            loc, eq, min, max, ..
        } => {
            let mut s = format!("expect_count: {{{}", loc_fields(loc)?);
            if let Some(e) = eq {
                s.push_str(&format!(", eq: {e}"));
            }
            if let Some(m) = min {
                s.push_str(&format!(", min: {m}"));
            }
            if let Some(m) = max {
                s.push_str(&format!(", max: {m}"));
            }
            s.push_str(&t);
            s.push('}');
            s
        }
        WaitCond::Exit => format!("expect_exit: {{timeout_ms: {timeout_ms}}}"),
    })
}

fn bytes_as_write(hex: &str) -> String {
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect();
    q(&String::from_utf8_lossy(&bytes))
}

/// Render the spec.
pub fn render(h: &SpecHeader, steps: &[Recorded]) -> String {
    let mut out = String::new();
    out.push_str(&format!("name: {}\n", q(h.name)));
    out.push_str("matrix:\n");
    out.push_str(&format!("  profiles: [{}]\n", h.profile));
    out.push_str(&format!("  sizes: [\"{}x{}\"]\n", h.cols, h.rows));
    out.push_str(&format!(
        "spawn: [{}]\n",
        h.argv.iter().map(|a| q(a)).collect::<Vec<_>>().join(", ")
    ));
    if !h.env.is_empty() {
        out.push_str("env:\n");
        for (k, v) in h.env {
            out.push_str(&format!("  {}: {}\n", q(k), q(v)));
        }
    }
    out.push_str("steps:\n");
    if steps.is_empty() {
        out.push_str("  []\n");
        return out;
    }
    for step in steps {
        let line = match step {
            Recorded::Input(Input::Text { text }) => format!("write: {}", q(text)),
            Recorded::Input(Input::Bytes { hex }) => format!("write: {}", bytes_as_write(hex)),
            Recorded::Input(Input::Key { key }) => key_step(key),
            Recorded::Input(Input::Paste { text }) => format!("paste: {}", q(text)),
            Recorded::Input(Input::Mouse { ev }) => mouse_step(ev),
            Recorded::Resize(c, r) => format!("resize: \"{c}x{r}\""),
            Recorded::Wait {
                cond,
                ok,
                timeout_ms,
            } => match wait_step(cond, *timeout_ms) {
                Some(s) if *ok => s,
                Some(s) => {
                    out.push_str(&format!("  # did not hold when recorded: - {s}\n"));
                    continue;
                }
                None => continue,
            },
        };
        out.push_str(&format!("  - {line}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_runner::spec::Spec;

    fn text(p: &str) -> Locator {
        Locator::Text {
            pattern: p.into(),
            ignore_case: false,
            whole_line: false,
        }
    }

    #[test]
    fn renders_a_spec_that_parses_and_runs() {
        let argv = vec![
            "sh".to_string(),
            "-c".into(),
            "read x; echo \"got $x\"; exec cat".into(),
        ];
        let env = vec![("TERM_X".to_string(), "1".to_string())];
        let h = SpecHeader {
            name: "demo \"q\"",
            argv: &argv,
            profile: "xterm",
            cols: 40,
            rows: 10,
            env: &env,
        };
        let steps = vec![
            Recorded::Input(Input::Text {
                text: "hi\n".into(),
            }),
            Recorded::Input(Input::Key {
                key: KeyEvent::with(Key::Char('c'), Mods::CTRL | Mods::ALT),
            }),
            Recorded::Input(Input::Key {
                key: KeyEvent::new(Key::F(5)),
            }),
            Recorded::Input(Input::Paste { text: "p".into() }),
            Recorded::Input(Input::Bytes {
                hex: "1b5b41".into(),
            }),
            Recorded::Input(Input::Mouse {
                ev: MouseEvent {
                    button: MouseButton::WheelDown,
                    action: MouseAction::Press,
                    row: 1,
                    col: 2,
                    mods: Mods::SHIFT,
                },
            }),
            Recorded::Resize(50, 12),
            Recorded::Wait {
                cond: WaitCond::Visible {
                    loc: text("got hi"),
                    multiline: false,
                },
                ok: true,
                timeout_ms: 2000,
            },
            Recorded::Wait {
                cond: WaitCond::NotVisible {
                    loc: text("nope"),
                    multiline: false,
                },
                ok: true,
                timeout_ms: 100,
            },
            Recorded::Wait {
                cond: WaitCond::Contains {
                    loc: Locator::Line { row: 0 },
                    contains: "got".into(),
                    multiline: false,
                },
                ok: true,
                timeout_ms: 100,
            },
            Recorded::Wait {
                cond: WaitCond::Count {
                    loc: Locator::Regex { re: "g.t".into() },
                    eq: None,
                    min: Some(1),
                    max: Some(3),
                    multiline: false,
                },
                ok: true,
                timeout_ms: 100,
            },
            Recorded::Wait {
                cond: WaitCond::Text {
                    loc: Locator::Cell { row: 0, col: 0 },
                    equals: "g".into(),
                    multiline: false,
                },
                ok: true,
                timeout_ms: 100,
            },
            Recorded::Wait {
                cond: WaitCond::Visible {
                    loc: text("never"),
                    multiline: false,
                },
                ok: false,
                timeout_ms: 10,
            },
        ];
        let yaml = render(&h, &steps);
        assert!(
            yaml.contains("# did not hold when recorded: - expect_visible"),
            "{yaml}"
        );
        assert!(
            yaml.contains("key: {key: \"c\", mods: [ctrl, alt]}"),
            "{yaml}"
        );
        assert!(yaml
            .contains("mouse: {row: 1, col: 2, button: wheel_down, action: press, mods: [shift]}"));
        let spec = Spec::from_yaml(&yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"));
        assert_eq!(spec.name, "demo \"q\"");
        assert_eq!(spec.spawn, argv);
        assert_eq!(spec.steps.len(), 12);
    }

    #[test]
    fn empty_steps_is_valid_yaml() {
        let h = SpecHeader {
            name: "n",
            argv: &["true".to_string()],
            profile: "vt220",
            cols: 80,
            rows: 24,
            env: &[],
        };
        let spec = Spec::from_yaml(&render(&h, &[])).unwrap();
        assert!(spec.steps.is_empty());
        assert_eq!(spec.matrix.profiles, vec!["vt220"]);
    }
}
