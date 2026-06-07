//! Snapshot data types (§11). Rendering lives in `muse-render`; the canonical
//! serialization of the styled tier lives here so diffing can reproduce it.

use crate::color::Color;
use crate::style::{Attrs, CellStyle};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotKind {
    Text,
    Styled,
    Pixel { scale: u8 },
}

/// One styled run within a row: a contiguous span of cells sharing a style.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StyleRun {
    pub start_col: u16,
    pub len: u16,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StyledRow {
    pub text: String,
    pub runs: Vec<StyleRun>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct StyledSnapshot {
    pub rows: Vec<StyledRow>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PixelSnapshot {
    pub width: u32,
    pub height: u32,
    /// PNG-encoded bytes.
    pub png: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Snapshot {
    Text(String),
    Styled(StyledSnapshot),
    Pixel(PixelSnapshot),
}

fn color_hex(c: Color) -> String {
    match c {
        Color::Default => "default".to_string(),
        Color::Indexed(i) => format!("idx{:02x}", i),
        Color::Rgb(r, g, b) => format!("#{:02x}{:02x}{:02x}", r, g, b),
    }
}

impl StyledSnapshot {
    /// Canonical, diff-friendly text serialization: per row the text line,
    /// then run lines `R <start> <len> fg=.. bg=.. attrs=..` with lowercase
    /// hex colors and sorted attribute names.
    pub fn to_canonical(&self) -> String {
        let mut s = String::new();
        for (i, row) in self.rows.iter().enumerate() {
            s.push_str(&format!("L{}|{}\n", i, row.text));
            for run in &row.runs {
                let attrs = run.attrs.names().join(",");
                s.push_str(&format!(
                    "R{} {} {} fg={} bg={} attrs={}\n",
                    i,
                    run.start_col,
                    run.len,
                    color_hex(run.fg),
                    color_hex(run.bg),
                    attrs
                ));
            }
        }
        s
    }
}

impl StyleRun {
    pub fn style(&self) -> CellStyle {
        CellStyle {
            fg: self.fg,
            bg: self.bg,
            underline: Color::Default,
            attrs: self.attrs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_format() {
        let snap = StyledSnapshot {
            rows: vec![StyledRow {
                text: "HI".into(),
                runs: vec![StyleRun {
                    start_col: 0,
                    len: 2,
                    fg: Color::Indexed(1),
                    bg: Color::Default,
                    attrs: Attrs::BOLD,
                }],
            }],
        };
        let c = snap.to_canonical();
        assert!(c.contains("L0|HI"));
        assert!(c.contains("R0 0 2 fg=idx01 bg=default attrs=BOLD"));
    }

    #[test]
    fn color_hex_forms() {
        assert_eq!(color_hex(Color::Default), "default");
        assert_eq!(color_hex(Color::Indexed(255)), "idxff");
        assert_eq!(color_hex(Color::Rgb(255, 0, 16)), "#ff0010");
    }

    #[test]
    fn run_to_style() {
        let r = StyleRun {
            start_col: 0,
            len: 1,
            fg: Color::Indexed(2),
            bg: Color::Default,
            attrs: Attrs::ITALIC,
        };
        assert_eq!(r.style().fg, Color::Indexed(2));
        assert_eq!(r.style().attrs, Attrs::ITALIC);
    }

    #[test]
    fn snapshot_kind_serde() {
        let k = SnapshotKind::Pixel { scale: 2 };
        let s = serde_json::to_string(&k).unwrap();
        assert_eq!(serde_json::from_str::<SnapshotKind>(&s).unwrap(), k);
    }

    #[test]
    fn empty_canonical() {
        assert_eq!(StyledSnapshot::default().to_canonical(), "");
    }
}
