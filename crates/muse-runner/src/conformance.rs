//! Conformance / differential harness (§20). The emulator corpus feeds bytes
//! to a fresh emulator and asserts the resulting screen; the protocol corpus
//! reuses the spec runner.

use muse_core::color::Color;
use muse_core::error::Result;
use muse_core::screen::Screen;
use muse_core::style::{Attrs, CellStyle};
use muse_emulator::{profile, Emulator, VtEmulator};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct EmulatorCase {
    pub name: String,
    #[serde(default = "xterm_name")]
    pub profile: String,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
    /// Bytes to feed, with `\e` for ESC and standard escapes.
    pub feed: String,
    pub expect: Expect,
}

fn xterm_name() -> String {
    "xterm".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Expect {
    #[serde(default)]
    pub cursor: Option<CursorExpect>,
    #[serde(default)]
    pub lines: Vec<LineExpect>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CursorExpect {
    pub row: u16,
    pub col: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LineExpect {
    pub text: String,
    #[serde(default)]
    pub styles: Vec<StyleExpect>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StyleExpect {
    pub start: u16,
    pub len: u16,
    #[serde(default)]
    pub fg: Option<ColorExpect>,
    #[serde(default)]
    pub bg: Option<ColorExpect>,
    #[serde(default)]
    pub attrs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorExpect {
    Default,
    Indexed(u8),
    Rgb([u8; 3]),
}

impl ColorExpect {
    fn to_color(&self) -> Color {
        match self {
            ColorExpect::Default => Color::Default,
            ColorExpect::Indexed(i) => Color::Indexed(*i),
            ColorExpect::Rgb([r, g, b]) => Color::Rgb(*r, *g, *b),
        }
    }
}

/// Decode `\e`, `\n`, `\r`, `\t`, `\xHH`, `\\` escapes in a feed string to bytes.
pub fn decode_feed(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('e') => out.push(0x1b),
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('0') => out.push(0),
            Some('a') => out.push(0x07),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let h: String = chars.by_ref().take(2).collect();
                if let Ok(b) = u8::from_str_radix(&h, 16) {
                    out.push(b);
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

fn check_style(actual: &CellStyle, exp: &StyleExpect) -> std::result::Result<(), String> {
    if let Some(fg) = &exp.fg {
        if actual.fg != fg.to_color() {
            return Err(format!(
                "fg mismatch: got {:?} want {:?}",
                actual.fg,
                fg.to_color()
            ));
        }
    }
    if let Some(bg) = &exp.bg {
        if actual.bg != bg.to_color() {
            return Err(format!(
                "bg mismatch: got {:?} want {:?}",
                actual.bg,
                bg.to_color()
            ));
        }
    }
    let mut want = Attrs::empty();
    for a in &exp.attrs {
        want |= Attrs::parse_name(a).ok_or_else(|| format!("unknown attr `{a}`"))?;
    }
    if !actual.attrs.contains(want) {
        return Err(format!(
            "attrs mismatch: got {:?} want {:?}",
            actual.attrs, want
        ));
    }
    Ok(())
}

fn verify(screen: &Screen, expect: &Expect) -> std::result::Result<(), String> {
    let grid = screen.active_grid();
    if let Some(cur) = &expect.cursor {
        if screen.cursor.row != cur.row || screen.cursor.col != cur.col {
            return Err(format!(
                "cursor mismatch: got ({},{}) want ({},{})",
                screen.cursor.row, screen.cursor.col, cur.row, cur.col
            ));
        }
    }
    for (r, line) in expect.lines.iter().enumerate() {
        let got = grid.row_text_trimmed(r as u16);
        if got != line.text {
            return Err(format!(
                "line {r} text mismatch: got {got:?} want {:?}",
                line.text
            ));
        }
        for st in &line.styles {
            for c in st.start..st.start + st.len {
                check_style(&grid.cell(r as u16, c).style, st)
                    .map_err(|e| format!("line {r} col {c}: {e}"))?;
            }
        }
    }
    Ok(())
}

/// Run one emulator corpus case; Ok(()) on pass.
pub fn run_emulator_case(case: &EmulatorCase) -> std::result::Result<(), String> {
    let prof =
        profile::by_name(&case.profile).ok_or_else(|| format!("bad profile {}", case.profile))?;
    let cols = case.cols.unwrap_or(80);
    let rows = case.rows.unwrap_or(24);
    let mut emu = VtEmulator::new(prof, cols, rows);
    emu.advance(&decode_feed(&case.feed));
    let screen = emu.snapshot_screen();
    verify(&screen, &case.expect)
}

/// Parse an emulator corpus YAML document (single case).
pub fn parse_emulator_case(yaml: &str) -> Result<EmulatorCase> {
    crate::spec::yaml_to(yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_escapes() {
        assert_eq!(decode_feed(r"\e[31m"), b"\x1b[31m");
        assert_eq!(decode_feed(r"a\nb"), b"a\nb");
        assert_eq!(decode_feed(r"\x41"), b"A");
        assert_eq!(decode_feed(r"\\"), b"\\");
        assert_eq!(decode_feed(r"\0"), vec![0]);
        assert_eq!(decode_feed(r"\a"), vec![0x07]);
        assert_eq!(decode_feed(r"\q"), b"\\q");
    }

    #[test]
    fn sgr_basic_corpus() {
        let case = parse_emulator_case(
            r#"
name: sgr_basic
profile: xterm
feed: "\e[1;31mHI\e[0m"
expect:
  cursor: {row: 0, col: 2}
  lines:
    - text: "HI"
      styles:
        - {start: 0, len: 2, fg: {indexed: 1}, attrs: [BOLD]}
"#,
        )
        .unwrap();
        assert!(run_emulator_case(&case).is_ok());
    }

    #[test]
    fn detects_text_mismatch() {
        let case = parse_emulator_case(
            r#"
name: bad
feed: "HI"
expect:
  lines:
    - text: "BYE"
"#,
        )
        .unwrap();
        assert!(run_emulator_case(&case).is_err());
    }

    #[test]
    fn detects_cursor_mismatch() {
        let case = parse_emulator_case(
            r#"
name: cur
feed: "HI"
expect:
  cursor: {row: 5, col: 5}
"#,
        )
        .unwrap();
        let err = run_emulator_case(&case).unwrap_err();
        assert!(err.contains("cursor"));
    }

    #[test]
    fn detects_style_mismatch() {
        let case = parse_emulator_case(
            r#"
name: sty
feed: "HI"
expect:
  lines:
    - text: "HI"
      styles:
        - {start: 0, len: 2, fg: {indexed: 9}}
"#,
        )
        .unwrap();
        assert!(run_emulator_case(&case).is_err());
    }

    #[test]
    fn rgb_and_bg_colors() {
        let case = parse_emulator_case(
            r#"
name: rgb
feed: "\e[38;2;1;2;3;48;5;4mX"
expect:
  lines:
    - text: "X"
      styles:
        - {start: 0, len: 1, fg: {rgb: [1,2,3]}, bg: {indexed: 4}}
"#,
        )
        .unwrap();
        assert!(
            run_emulator_case(&case).is_ok(),
            "{:?}",
            run_emulator_case(&case)
        );
    }

    #[test]
    fn default_color_expect() {
        let case = parse_emulator_case(
            r#"
name: def
feed: "X"
expect:
  lines:
    - text: "X"
      styles:
        - {start: 0, len: 1, fg: default}
"#,
        )
        .unwrap();
        assert!(run_emulator_case(&case).is_ok());
    }

    #[test]
    fn bad_profile_errs() {
        let case =
            parse_emulator_case("name: x\nprofile: nope\nfeed: \"X\"\nexpect: {}\n").unwrap();
        assert!(run_emulator_case(&case).is_err());
    }

    #[test]
    fn unknown_attr_errs() {
        let case = parse_emulator_case(
            r#"
name: x
feed: "X"
expect:
  lines:
    - text: "X"
      styles:
        - {start: 0, len: 1, attrs: [WAT]}
"#,
        )
        .unwrap();
        assert!(run_emulator_case(&case).is_err());
    }

    #[test]
    fn custom_dims() {
        let case =
            parse_emulator_case("name: d\ncols: 10\nrows: 3\nfeed: \"X\"\nexpect: {}\n").unwrap();
        assert!(run_emulator_case(&case).is_ok());
    }

    #[test]
    fn parse_error() {
        assert!(parse_emulator_case("\t: : :").is_err());
    }
}
