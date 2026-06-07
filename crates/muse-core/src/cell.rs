//! A single display cell.

use crate::style::CellStyle;
use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

/// The content kind of a cell.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    /// No content (blank).
    Empty,
    /// A grapheme cluster occupying one or two columns.
    Glyph(CompactString),
    /// The trailing column of a wide (2-col) glyph.
    Spacer,
}

/// One display position.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Cell {
    pub kind: CellKind,
    pub style: CellStyle,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            kind: CellKind::Empty,
            style: CellStyle::default(),
        }
    }
}

impl Cell {
    pub fn empty() -> Self {
        Cell::default()
    }

    pub fn glyph(s: impl Into<CompactString>, style: CellStyle) -> Self {
        Cell {
            kind: CellKind::Glyph(s.into()),
            style,
        }
    }

    /// Display width: Spacer = 0, Empty = 1 (a blank column), Glyph = its
    /// unicode display width (1 or 2).
    pub fn width(&self) -> u8 {
        match &self.kind {
            CellKind::Spacer => 0,
            CellKind::Empty => 1,
            CellKind::Glyph(s) => {
                let w = UnicodeWidthStr::width(s.as_str());
                if w >= 2 {
                    2
                } else {
                    1
                }
            }
        }
    }

    /// The text contributed by this cell when building row strings.
    /// Empty contributes a space, Spacer contributes nothing, Glyph its text.
    pub fn text(&self) -> &str {
        match &self.kind {
            CellKind::Empty => " ",
            CellKind::Spacer => "",
            CellKind::Glyph(s) => s.as_str(),
        }
    }

    /// True if the cell has no glyph (Empty or Spacer).
    pub fn is_blank(&self) -> bool {
        !matches!(self.kind, CellKind::Glyph(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_width_one() {
        assert_eq!(Cell::empty().width(), 1);
        assert_eq!(Cell::empty().text(), " ");
        assert!(Cell::empty().is_blank());
    }

    #[test]
    fn narrow_glyph() {
        let c = Cell::glyph("a", CellStyle::default());
        assert_eq!(c.width(), 1);
        assert_eq!(c.text(), "a");
        assert!(!c.is_blank());
    }

    #[test]
    fn wide_glyph() {
        let c = Cell::glyph("日", CellStyle::default());
        assert_eq!(c.width(), 2);
        assert_eq!(c.text(), "日");
    }

    #[test]
    fn spacer() {
        let c = Cell {
            kind: CellKind::Spacer,
            style: CellStyle::default(),
        };
        assert_eq!(c.width(), 0);
        assert_eq!(c.text(), "");
        assert!(c.is_blank());
    }

    #[test]
    fn combining_attached_glyph_is_narrow() {
        // 'e' + combining acute => width 1
        let c = Cell::glyph("e\u{0301}", CellStyle::default());
        assert_eq!(c.width(), 1);
    }

    #[test]
    fn serde_roundtrip() {
        let c = Cell::glyph("x", CellStyle::default());
        let s = serde_json::to_string(&c).unwrap();
        let back: Cell = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }
}
