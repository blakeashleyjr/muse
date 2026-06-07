//! Test spec format (parsed from YAML/JSON) and conversions to domain types.

use muse_core::error::{Error, Result};
use muse_core::grid::Rect;
use muse_core::input::{Key, KeyEvent, Mods};
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
    Key(KeySpec),
    Resize(String),
    ExpectVisible(LocSpec),
    ExpectText(ExpectText),
    ExpectContains(ExpectContains),
    Snapshot(SnapSpec),
    SleepMs(u64),
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
        let mut mods = Mods::empty();
        for m in &self.mods {
            match m.to_lowercase().as_str() {
                "ctrl" | "control" => mods |= Mods::CTRL,
                "alt" | "meta" => mods |= Mods::ALT,
                "shift" => mods |= Mods::SHIFT,
                "super" | "cmd" => mods |= Mods::SUPER,
                other => return Err(Error::BadArgument(format!("unknown mod `{other}`"))),
            }
        }
        Ok(KeyEvent::with(key, mods))
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

#[derive(Debug, Clone, Deserialize)]
pub struct SnapSpec {
    pub name: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default = "one")]
    pub scale: u8,
    #[serde(default)]
    pub masks: Vec<MaskSpec>,
    #[serde(default)]
    pub normalize: Vec<NormalizeSpec>,
}

fn default_kind() -> String {
    "text".into()
}
fn one() -> u8 {
    1
}

impl SnapSpec {
    pub fn snapshot_kind(&self) -> SnapshotKind {
        match self.kind.as_str() {
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
  - key: {key: enter}
  - key: {key: c, mods: [ctrl]}
  - resize: "100x40"
  - expect_visible: {text: "hi"}
  - expect_text: {line: 0, equals: "hi"}
  - expect_contains: {regex: "h.", contains: "hi"}
  - snapshot: {name: s1, kind: text}
  - sleep_ms: 10
"#,
        )
        .unwrap();
        assert_eq!(spec.name, "demo");
        assert_eq!(spec.matrix.profiles_or_default(), vec!["xterm", "vt220"]);
        assert_eq!(spec.matrix.sizes_or_default(), vec![(80, 24), (100, 30)]);
        assert_eq!(spec.steps.len(), 9);
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
        // yaml_to routes through serde_json::Value
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
            kind: "pixel".into(),
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
        assert_eq!(s.snapshot_kind(), SnapshotKind::Pixel { scale: 2 });
        assert_eq!(s.mask_rules().len(), 1);
        assert_eq!(s.normalize_rules().len(), 1);
        let styled = SnapSpec {
            kind: "styled".into(),
            ..s.clone()
        };
        assert_eq!(styled.snapshot_kind(), SnapshotKind::Styled);
        let text = SnapSpec {
            kind: "text".into(),
            ..s
        };
        assert_eq!(text.snapshot_kind(), SnapshotKind::Text);
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
}
