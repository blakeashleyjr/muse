//! Test spec format (parsed from YAML/JSON) and conversions to domain types.

use muse_core::color::Color;
use muse_core::error::{Error, Result};
use muse_core::grid::Rect;
use muse_core::input::{Key, KeyEvent, Mods, MouseAction, MouseButton, MouseEvent};
use muse_core::locator::Locator;
use muse_core::snapshot::SnapshotKind;
use muse_diff::normalize::{MaskRule, NormalizeRule};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct Spec {
    pub name: String,
    #[serde(default)]
    pub matrix: Matrix,
    pub spawn: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Per-case temp dir: injected as this env var name; `{case_tmp}` expands in values/paths.
    #[serde(default)]
    pub case_tmp_env: Option<String>,
    /// Shared snapshot defaults applied to every `snapshot` step in this spec.
    #[serde(default)]
    pub snapshot_defaults: Option<SnapDefaults>,
    /// Override sync config for this spec (applied on top of `RunOpts::sync`).
    #[serde(default)]
    pub sync: Option<SyncSpec>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Matrix {
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub sizes: Vec<String>,
}

impl Matrix {
    pub fn profiles_or_default(&self) -> Vec<String> {
        if self.profiles.is_empty() {
            vec!["xterm".to_string()]
        } else {
            self.profiles.clone()
        }
    }
    pub fn sizes_or_default(&self) -> Vec<(u16, u16)> {
        let raw = if self.sizes.is_empty() {
            vec!["80x24".to_string()]
        } else {
            self.sizes.clone()
        };
        raw.iter().filter_map(|s| parse_size(s)).collect()
    }
}

pub fn parse_size(s: &str) -> Option<(u16, u16)> {
    let (c, r) = s.split_once('x')?;
    Some((c.trim().parse().ok()?, r.trim().parse().ok()?))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Write(String),
    Paste(String),
    /// Write `s` followed by a newline.
    WriteLine(String),
    Key(KeySpec),
    Resize(String),
    ExpectVisible(LocSpec),
    /// Fail if the locator still matches after `timeout_ms` (default: deadline).
    ExpectNotVisible(LocSpec),
    ExpectText(ExpectText),
    ExpectContains(ExpectContains),
    /// Assert the number of matches for a locator.
    ExpectCount(ExpectCountSpec),
    Snapshot(SnapSpec),
    SleepMs(u64),
    /// Read a file and fail if any line matches `reject_re`.
    CheckFile(CheckFileSpec),
    /// Wait for the SUT to exit and assert the exit code.
    ExpectExit(ExpectExitSpec),
    /// Assert style properties on a matched locator.
    ExpectStyle(ExpectStyleSpec),
    /// Send a mouse event.
    Mouse(MouseSpec),
    /// Label the next trace section (calls end_step + begin_step on the terminal).
    BeginStep(String),
    /// Like `check_file` but only inspects lines appended since the last call.
    /// Maintains a per-path read cursor across steps so you get mid-test checkpoints
    /// rather than a full file scan at the end.
    WatchLog(CheckFileSpec),
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeySpec {
    pub key: String,
    #[serde(default)]
    pub mods: Vec<String>,
}

impl KeySpec {
    pub fn to_event(&self) -> Result<KeyEvent> {
        let key = match self.key.as_str() {
            "enter" => Key::Enter,
            "tab" => Key::Tab,
            "backspace" => Key::Backspace,
            "escape" | "esc" => Key::Escape,
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "insert" => Key::Insert,
            "delete" => Key::Delete,
            s if s.starts_with('f') && s[1..].parse::<u8>().is_ok() => {
                Key::F(s[1..].parse().unwrap())
            }
            s if s.chars().count() == 1 => Key::Char(s.chars().next().unwrap()),
            other => return Err(Error::BadArgument(format!("unknown key `{other}`"))),
        };
        Ok(KeyEvent::with(key, parse_mods(&self.mods)?))
    }
}

/// Parse modifier names (`ctrl`/`control`, `alt`/`meta`, `shift`, `super`/`cmd`).
pub fn parse_mods(names: &[String]) -> Result<Mods> {
    let mut mods = Mods::empty();
    for m in names {
        match m.to_lowercase().as_str() {
            "ctrl" | "control" => mods |= Mods::CTRL,
            "alt" | "meta" => mods |= Mods::ALT,
            "shift" => mods |= Mods::SHIFT,
            "super" | "cmd" => mods |= Mods::SUPER,
            other => return Err(Error::BadArgument(format!("unknown mod `{other}`"))),
        }
    }
    Ok(mods)
}

impl MouseSpec {
    /// Build the event; unknown buttons/actions/mods are errors, not silently
    /// coerced to left-press.
    pub fn to_event(&self) -> Result<MouseEvent> {
        let button = match self.button.to_lowercase().as_str() {
            "left" => MouseButton::Left,
            "right" => MouseButton::Right,
            "middle" => MouseButton::Middle,
            "wheel_up" | "wheelup" => MouseButton::WheelUp,
            "wheel_down" | "wheeldown" => MouseButton::WheelDown,
            other => {
                return Err(Error::BadArgument(format!(
                    "unknown mouse button `{other}`"
                )))
            }
        };
        let action = match self.action.to_lowercase().as_str() {
            "press" => MouseAction::Press,
            "release" => MouseAction::Release,
            "move" => MouseAction::Move,
            other => {
                return Err(Error::BadArgument(format!(
                    "unknown mouse action `{other}`"
                )))
            }
        };
        Ok(MouseEvent {
            button,
            action,
            row: self.row,
            col: self.col,
            mods: parse_mods(&self.mods)?,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LocSpec {
    pub text: Option<String>,
    pub regex: Option<String>,
    pub line: Option<u16>,
    pub cell: Option<[u16; 2]>,
    pub region: Option<[u16; 4]>,
    #[serde(default)]
    pub cursor: bool,
    #[serde(default)]
    pub ignore_case: bool,
    #[serde(default)]
    pub whole_line: bool,
    #[serde(default)]
    pub multiline: bool,
    /// Per-step deadline override. Falls back to `RunOpts::assert_deadline_ms`.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl LocSpec {
    pub fn to_locator(&self) -> Result<(Locator, bool)> {
        let loc = if let Some(t) = &self.text {
            Locator::Text {
                pattern: t.clone(),
                ignore_case: self.ignore_case,
                whole_line: self.whole_line,
            }
        } else if let Some(re) = &self.regex {
            Locator::Regex { re: re.clone() }
        } else if let Some(l) = self.line {
            Locator::Line { row: l }
        } else if let Some([r, c]) = self.cell {
            Locator::Cell { row: r, col: c }
        } else if let Some([r, c, w, h]) = self.region {
            Locator::Region {
                rect: Rect::new(r, c, w, h),
            }
        } else if self.cursor {
            Locator::Cursor
        } else {
            return Err(Error::BadArgument("empty locator".into()));
        };
        Ok((loc, self.multiline))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExpectText {
    #[serde(flatten)]
    pub loc: LocSpec,
    pub equals: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExpectContains {
    #[serde(flatten)]
    pub loc: LocSpec,
    pub contains: String,
}

/// Assert match count is within a range or equal to a specific value.
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectCountSpec {
    #[serde(flatten)]
    pub loc: LocSpec,
    /// Exact count required.
    pub eq: Option<usize>,
    /// Minimum count (inclusive).
    pub min: Option<usize>,
    /// Maximum count (inclusive).
    pub max: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SnapSpec {
    pub name: String,
    /// Snapshot kind: `text` (default), `styled`, or `pixel`. Falls back to `snapshot_defaults.kind`.
    pub kind: Option<String>,
    #[serde(default = "one")]
    pub scale: u8,
    #[serde(default)]
    pub masks: Vec<MaskSpec>,
    #[serde(default)]
    pub normalize: Vec<NormalizeSpec>,
}

fn one() -> u8 {
    1
}

impl SnapSpec {
    /// Effective snapshot kind, using `default_kind` (from `snapshot_defaults`) when unset.
    pub fn snapshot_kind(&self, default_kind: Option<&str>) -> SnapshotKind {
        match self.kind.as_deref().or(default_kind).unwrap_or("text") {
            "styled" => SnapshotKind::Styled,
            "pixel" => SnapshotKind::Pixel { scale: self.scale },
            _ => SnapshotKind::Text,
        }
    }
    pub fn mask_rules(&self) -> Vec<MaskRule> {
        self.masks.iter().filter_map(|m| m.to_rule()).collect()
    }
    pub fn normalize_rules(&self) -> Vec<NormalizeRule> {
        self.normalize
            .iter()
            .map(|n| NormalizeRule {
                re: n.re.clone(),
                replace: n.replace.clone(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MaskSpec {
    pub rect: Option<[u16; 4]>,
    pub content: Option<String>,
}

impl MaskSpec {
    pub fn to_rule(&self) -> Option<MaskRule> {
        if let Some([r, c, w, h]) = self.rect {
            Some(MaskRule::Rect(Rect::new(r, c, w, h)))
        } else {
            self.content.clone().map(MaskRule::Content)
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NormalizeSpec {
    pub re: String,
    pub replace: String,
}

/// Spec-level snapshot defaults: applied to every `snapshot` step unless overridden per-step.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SnapDefaults {
    /// Default kind (`text`, `styled`, `pixel`) for snapshots that don't specify one.
    pub kind: Option<String>,
    /// Extra masks merged into every snapshot's mask list.
    #[serde(default)]
    pub masks: Vec<MaskSpec>,
    /// Extra normalize rules merged into every snapshot's normalize list.
    #[serde(default)]
    pub normalize: Vec<NormalizeSpec>,
}

impl SnapDefaults {
    pub fn mask_rules(&self) -> Vec<MaskRule> {
        self.masks.iter().filter_map(|m| m.to_rule()).collect()
    }
    pub fn normalize_rules(&self) -> Vec<NormalizeRule> {
        self.normalize
            .iter()
            .map(|n| NormalizeRule {
                re: n.re.clone(),
                replace: n.replace.clone(),
            })
            .collect()
    }
}

/// Per-spec sync tuning, applied on top of `RunOpts::sync`.
#[derive(Debug, Clone, Deserialize)]
pub struct SyncSpec {
    /// Milliseconds of silence required before declaring a stable frame.
    pub quiet_window_ms: Option<u64>,
    /// Hard cap on settle wait (overrides the global max_settle_ms).
    pub max_settle_ms: Option<u64>,
}

/// Read a file; fail the step if any line matches `reject_re`.
#[derive(Debug, Clone, Deserialize)]
pub struct CheckFileSpec {
    /// File path. Supports `{case_tmp}` expansion.
    pub path: String,
    /// A regex — any matching line causes the step to fail.
    pub reject_re: String,
    /// If true (the default), silently pass when the file doesn't exist yet.
    #[serde(default = "bool_true")]
    pub skip_if_missing: bool,
}

/// Wait for the SUT to exit and assert its exit code.
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectExitSpec {
    pub code: i32,
    #[serde(default = "default_exit_timeout")]
    pub timeout_ms: u64,
}

/// Assert style attributes on a matched region.
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectStyleSpec {
    #[serde(flatten)]
    pub loc: LocSpec,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub dim: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub reverse: Option<bool>,
    /// Foreground color: `"default"`, `"indexed(N)"`, or `"rgb(r,g,b)"`.
    pub fg: Option<String>,
    /// Background color: same format as `fg`.
    pub bg: Option<String>,
}

/// Send a mouse event to the terminal.
#[derive(Debug, Clone, Deserialize)]
pub struct MouseSpec {
    pub row: u16,
    pub col: u16,
    /// `"left"` (default), `"right"`, `"middle"`, `"wheel_up"`, `"wheel_down"`.
    #[serde(default = "default_button")]
    pub button: String,
    /// `"press"` (default), `"release"`, or `"move"`.
    #[serde(default = "default_action")]
    pub action: String,
    #[serde(default)]
    pub mods: Vec<String>,
}

// ── Default helpers ────────────────────────────────────────────────────────────

fn bool_true() -> bool {
    true
}
fn default_exit_timeout() -> u64 {
    3000
}
fn default_button() -> String {
    "left".into()
}
fn default_action() -> String {
    "press".into()
}

// ── Color parsing ──────────────────────────────────────────────────────────────

/// Parse a color string: `"default"`, `"indexed(N)"`, or `"rgb(r,g,b)"`.
pub fn parse_color(s: &str) -> Result<Color> {
    let s = s.trim();
    if s == "default" {
        return Ok(Color::Default);
    }
    if let Some(inner) = s.strip_prefix("indexed(").and_then(|t| t.strip_suffix(')')) {
        let n: u8 = inner
            .trim()
            .parse()
            .map_err(|_| Error::BadArgument(format!("bad indexed color: {s:?}")))?;
        return Ok(Color::Indexed(n));
    }
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|t| t.strip_suffix(')')) {
        let parts: Vec<&str> = inner.splitn(3, ',').collect();
        if parts.len() == 3 {
            let parse_ch = |p: &str| -> Result<u8> {
                p.trim()
                    .parse()
                    .map_err(|_| Error::BadArgument(format!("bad rgb component in {s:?}")))
            };
            return Ok(Color::Rgb(
                parse_ch(parts[0])?,
                parse_ch(parts[1])?,
                parse_ch(parts[2])?,
            ));
        }
    }
    Err(Error::BadArgument(format!(
        "unknown color {s:?} — use \"default\", \"indexed(N)\", or \"rgb(r,g,b)\""
    )))
}

// ── YAML/JSON parsing ──────────────────────────────────────────────────────────

/// Parse YAML into `T` by way of `serde_json::Value`. This gives JSON-style
/// externally-tagged enum handling (single-key maps like `{write: "x"}` and
/// `{indexed: 1}`), which serde_yaml 0.9 otherwise represents with `!tags`.
pub fn yaml_to<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    let value: serde_json::Value =
        serde_yaml::from_str(s).map_err(|e| Error::BadArgument(format!("yaml parse: {e}")))?;
    serde_json::from_value(value).map_err(|e| Error::BadArgument(format!("spec parse: {e}")))
}

impl Spec {
    pub fn from_yaml(s: &str) -> Result<Spec> {
        yaml_to(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_spec() {
        let spec = Spec::from_yaml(
            r#"
name: demo
matrix:
  profiles: [xterm, vt220]
  sizes: ["80x24", "100x30"]
spawn: ["echo", "hi"]
env:
  FOO: bar
steps:
  - write: "ls\n"
  - write_line: "ls"
  - key: {key: enter}
  - key: {key: c, mods: [ctrl]}
  - resize: "100x40"
  - expect_visible: {text: "hi"}
  - expect_not_visible: {text: "gone"}
  - expect_text: {line: 0, equals: "hi"}
  - expect_contains: {regex: "h.", contains: "hi"}
  - expect_count: {text: "hi", eq: 1}
  - snapshot: {name: s1}
  - sleep_ms: 10
  - mouse: {row: 5, col: 10}
  - begin_step: "my section"
"#,
        )
        .unwrap();
        assert_eq!(spec.name, "demo");
        assert_eq!(spec.matrix.profiles_or_default(), vec!["xterm", "vt220"]);
        assert_eq!(spec.matrix.sizes_or_default(), vec![(80, 24), (100, 30)]);
        assert_eq!(spec.steps.len(), 14);
        assert_eq!(spec.env.get("FOO").unwrap(), "bar");
    }

    #[test]
    fn defaults_when_no_matrix() {
        let spec = Spec::from_yaml("name: x\nspawn: [echo]\n").unwrap();
        assert_eq!(spec.matrix.profiles_or_default(), vec!["xterm"]);
        assert_eq!(spec.matrix.sizes_or_default(), vec![(80, 24)]);
    }

    #[test]
    fn parse_size_helper() {
        assert_eq!(parse_size("80x24"), Some((80, 24)));
        assert_eq!(parse_size("bad"), None);
    }

    #[test]
    fn key_specials_and_chars() {
        assert_eq!(
            KeySpec {
                key: "enter".into(),
                mods: vec![]
            }
            .to_event()
            .unwrap()
            .key,
            Key::Enter
        );
        assert_eq!(
            KeySpec {
                key: "f5".into(),
                mods: vec![]
            }
            .to_event()
            .unwrap()
            .key,
            Key::F(5)
        );
        let ev = KeySpec {
            key: "c".into(),
            mods: vec!["ctrl".into()],
        }
        .to_event()
        .unwrap();
        assert_eq!(ev.key, Key::Char('c'));
        assert!(ev.mods.contains(Mods::CTRL));
    }

    #[test]
    fn all_special_keys_parse() {
        for (name, want) in [
            ("enter", Key::Enter),
            ("tab", Key::Tab),
            ("backspace", Key::Backspace),
            ("escape", Key::Escape),
            ("esc", Key::Escape),
            ("up", Key::Up),
            ("down", Key::Down),
            ("left", Key::Left),
            ("right", Key::Right),
            ("home", Key::Home),
            ("end", Key::End),
            ("pageup", Key::PageUp),
            ("pagedown", Key::PageDown),
            ("insert", Key::Insert),
            ("delete", Key::Delete),
            ("f1", Key::F(1)),
            ("f12", Key::F(12)),
        ] {
            let ev = KeySpec {
                key: name.into(),
                mods: vec![],
            }
            .to_event()
            .unwrap();
            assert_eq!(ev.key, want, "{name}");
        }
    }

    #[test]
    fn all_mods_parse() {
        let ev = KeySpec {
            key: "a".into(),
            mods: vec!["ctrl".into(), "alt".into(), "shift".into(), "super".into()],
        }
        .to_event()
        .unwrap();
        assert!(ev
            .mods
            .contains(Mods::CTRL | Mods::ALT | Mods::SHIFT | Mods::SUPER));
        // aliases
        let ev2 = KeySpec {
            key: "b".into(),
            mods: vec!["control".into(), "meta".into(), "cmd".into()],
        }
        .to_event()
        .unwrap();
        assert!(ev2.mods.contains(Mods::CTRL | Mods::ALT | Mods::SUPER));
    }

    #[test]
    fn from_json_value_path() {
        let s: Spec = yaml_to("name: j\nspawn: [echo]\n").unwrap();
        assert_eq!(s.name, "j");
    }

    #[test]
    fn key_errors() {
        assert!(KeySpec {
            key: "nope".into(),
            mods: vec![]
        }
        .to_event()
        .is_err());
        assert!(KeySpec {
            key: "a".into(),
            mods: vec!["bogus".into()]
        }
        .to_event()
        .is_err());
    }

    #[test]
    fn loc_variants() {
        assert!(matches!(
            LocSpec {
                text: Some("x".into()),
                ..Default::default()
            }
            .to_locator()
            .unwrap()
            .0,
            Locator::Text { .. }
        ));
        assert!(matches!(
            LocSpec {
                regex: Some("x".into()),
                ..Default::default()
            }
            .to_locator()
            .unwrap()
            .0,
            Locator::Regex { .. }
        ));
        assert!(matches!(
            LocSpec {
                line: Some(1),
                ..Default::default()
            }
            .to_locator()
            .unwrap()
            .0,
            Locator::Line { .. }
        ));
        assert!(matches!(
            LocSpec {
                cell: Some([1, 2]),
                ..Default::default()
            }
            .to_locator()
            .unwrap()
            .0,
            Locator::Cell { .. }
        ));
        assert!(matches!(
            LocSpec {
                region: Some([0, 0, 1, 1]),
                ..Default::default()
            }
            .to_locator()
            .unwrap()
            .0,
            Locator::Region { .. }
        ));
        assert!(matches!(
            LocSpec {
                cursor: true,
                ..Default::default()
            }
            .to_locator()
            .unwrap()
            .0,
            Locator::Cursor
        ));
        assert!(LocSpec::default().to_locator().is_err());
    }

    #[test]
    fn snap_spec_kinds() {
        let s = SnapSpec {
            name: "a".into(),
            kind: Some("pixel".into()),
            scale: 2,
            masks: vec![MaskSpec {
                rect: Some([0, 0, 1, 1]),
                content: None,
            }],
            normalize: vec![NormalizeSpec {
                re: "x".into(),
                replace: "y".into(),
            }],
        };
        assert_eq!(s.snapshot_kind(None), SnapshotKind::Pixel { scale: 2 });
        assert_eq!(s.mask_rules().len(), 1);
        assert_eq!(s.normalize_rules().len(), 1);

        let styled = SnapSpec {
            kind: Some("styled".into()),
            ..s.clone()
        };
        assert_eq!(styled.snapshot_kind(None), SnapshotKind::Styled);

        let text = SnapSpec {
            kind: Some("text".into()),
            ..s.clone()
        };
        assert_eq!(text.snapshot_kind(None), SnapshotKind::Text);

        // Default falls back to text when kind is None and no spec default
        let none_kind = SnapSpec {
            kind: None,
            ..s.clone()
        };
        assert_eq!(none_kind.snapshot_kind(None), SnapshotKind::Text);

        // Spec default is applied when kind is None
        assert_eq!(
            none_kind.snapshot_kind(Some("styled")),
            SnapshotKind::Styled
        );
    }

    #[test]
    fn mask_content_rule() {
        let m = MaskSpec {
            rect: None,
            content: Some(r"\d+".into()),
        };
        assert!(matches!(m.to_rule(), Some(MaskRule::Content(_))));
        let none = MaskSpec {
            rect: None,
            content: None,
        };
        assert!(none.to_rule().is_none());
    }

    #[test]
    fn bad_yaml_errors() {
        assert!(Spec::from_yaml("\t not yaml : : :").is_err());
    }

    #[test]
    fn parse_color_variants() {
        assert_eq!(parse_color("default").unwrap(), Color::Default);
        assert_eq!(parse_color("indexed(0)").unwrap(), Color::Indexed(0));
        assert_eq!(parse_color("indexed(255)").unwrap(), Color::Indexed(255));
        assert_eq!(
            parse_color("rgb(255, 0, 128)").unwrap(),
            Color::Rgb(255, 0, 128)
        );
        assert_eq!(parse_color("rgb(0,0,0)").unwrap(), Color::Rgb(0, 0, 0));
        assert!(parse_color("indexed(999)").is_err());
        assert!(parse_color("badcolor").is_err());
        assert!(parse_color("rgb(1,2)").is_err());
    }

    #[test]
    fn snap_defaults_merge() {
        let spec = Spec::from_yaml(
            r#"
name: x
spawn: [echo]
snapshot_defaults:
  kind: styled
  masks:
    - {rect: [0, 0, 5, 1]}
steps:
  - snapshot: {name: snap1}
  - snapshot: {name: snap2, kind: pixel}
"#,
        )
        .unwrap();
        let d = spec.snapshot_defaults.as_ref().unwrap();
        assert_eq!(d.kind.as_deref(), Some("styled"));
        assert_eq!(d.masks.len(), 1);
        // snap1 has no kind → defaults to styled
        if let Step::Snapshot(s) = &spec.steps[0] {
            assert_eq!(s.snapshot_kind(d.kind.as_deref()), SnapshotKind::Styled);
        }
        // snap2 overrides to pixel
        if let Step::Snapshot(s) = &spec.steps[1] {
            assert_eq!(
                s.snapshot_kind(d.kind.as_deref()),
                SnapshotKind::Pixel { scale: 1 }
            );
        }
    }

    #[test]
    fn sync_spec_parsed() {
        let spec = Spec::from_yaml(
            r#"
name: x
spawn: [echo]
sync:
  quiet_window_ms: 30
  max_settle_ms: 4000
"#,
        )
        .unwrap();
        let s = spec.sync.unwrap();
        assert_eq!(s.quiet_window_ms, Some(30));
        assert_eq!(s.max_settle_ms, Some(4000));
    }

    #[test]
    fn loc_timeout_ms_parsed() {
        let spec = Spec::from_yaml(
            r#"
name: x
spawn: [echo]
steps:
  - expect_visible: {text: "hi", timeout_ms: 8000}
  - expect_not_visible: {text: "bye", timeout_ms: 500}
"#,
        )
        .unwrap();
        if let Step::ExpectVisible(loc) = &spec.steps[0] {
            assert_eq!(loc.timeout_ms, Some(8000));
        } else {
            panic!("not ExpectVisible")
        }
        if let Step::ExpectNotVisible(loc) = &spec.steps[1] {
            assert_eq!(loc.timeout_ms, Some(500));
        } else {
            panic!("not ExpectNotVisible")
        }
    }

    #[test]
    fn expect_style_full_attrs() {
        let spec = Spec::from_yaml(
            r#"
name: x
spawn: [echo]
steps:
  - expect_style:
      text: "bold red"
      bold: true
      underline: true
      strike: true
      reverse: true
      fg: "indexed(1)"
      bg: "rgb(0, 0, 0)"
      timeout_ms: 2000
"#,
        )
        .unwrap();
        if let Step::ExpectStyle(s) = &spec.steps[0] {
            assert_eq!(s.bold, Some(true));
            assert_eq!(s.underline, Some(true));
            assert_eq!(s.strike, Some(true));
            assert_eq!(s.reverse, Some(true));
            assert_eq!(s.fg.as_deref(), Some("indexed(1)"));
            assert_eq!(s.bg.as_deref(), Some("rgb(0, 0, 0)"));
            assert_eq!(s.loc.timeout_ms, Some(2000));
        } else {
            panic!("not ExpectStyle")
        }
    }

    #[test]
    fn mouse_spec_defaults() {
        let spec = Spec::from_yaml(
            r#"
name: x
spawn: [echo]
steps:
  - mouse: {row: 5, col: 10}
"#,
        )
        .unwrap();
        if let Step::Mouse(m) = &spec.steps[0] {
            assert_eq!(m.row, 5);
            assert_eq!(m.col, 10);
            assert_eq!(m.button, "left");
            assert_eq!(m.action, "press");
        } else {
            panic!("not Mouse")
        }
    }

    #[test]
    fn expect_count_spec() {
        let spec = Spec::from_yaml(
            r#"
name: x
spawn: [echo]
steps:
  - expect_count: {text: "x", eq: 2}
  - expect_count: {text: "x", min: 1, max: 5}
"#,
        )
        .unwrap();
        if let Step::ExpectCount(c) = &spec.steps[0] {
            assert_eq!(c.eq, Some(2));
        } else {
            panic!("not ExpectCount")
        }
        if let Step::ExpectCount(c) = &spec.steps[1] {
            assert_eq!(c.min, Some(1));
            assert_eq!(c.max, Some(5));
        } else {
            panic!("not ExpectCount")
        }
    }

    #[test]
    fn mouse_spec_rejects_unknown_button_action_mod() {
        let ok = MouseSpec {
            row: 1,
            col: 2,
            button: "Right".into(),
            action: "release".into(),
            mods: vec!["ctrl".into()],
        };
        let ev = ok.to_event().unwrap();
        assert_eq!(ev.button, MouseButton::Right);
        assert_eq!(ev.action, MouseAction::Release);
        assert!(ev.mods.contains(Mods::CTRL));
        for (b, a, m) in [
            ("lft", "press", ""),
            ("left", "click", ""),
            ("left", "press", "hyper"),
        ] {
            let bad = MouseSpec {
                row: 0,
                col: 0,
                button: b.into(),
                action: a.into(),
                mods: if m.is_empty() { vec![] } else { vec![m.into()] },
            };
            assert!(bad.to_event().is_err(), "{b}/{a}/{m}");
        }
    }
}
