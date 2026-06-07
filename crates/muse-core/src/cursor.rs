//! Cursor position, visibility and shape.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    pub shape: CursorShape,
}

impl Default for Cursor {
    fn default() -> Self {
        Cursor {
            row: 0,
            col: 0,
            visible: true,
            shape: CursorShape::Block,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cursor() {
        let c = Cursor::default();
        assert_eq!((c.row, c.col), (0, 0));
        assert!(c.visible);
        assert_eq!(c.shape, CursorShape::Block);
    }

    #[test]
    fn shape_default() {
        assert_eq!(CursorShape::default(), CursorShape::Block);
    }

    #[test]
    fn serde_roundtrip() {
        let c = Cursor {
            row: 3,
            col: 4,
            visible: false,
            shape: CursorShape::Bar,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Cursor>(&s).unwrap(), c);
    }
}
