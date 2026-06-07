//! The full screen model: primary + alt grid, scrollback, cursor, modes.

use crate::cell::Cell;
use crate::cursor::Cursor;
use crate::grid::Grid;
use crate::modes::ModeState;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenKind {
    #[default]
    Primary,
    Alt,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Screen {
    pub primary: Grid,
    pub alt: Grid,
    pub active: ScreenKind,
    /// Bounded ring of scrolled-off rows; newest last.
    pub scrollback: Vec<Vec<Cell>>,
    pub cursor: Cursor,
    pub modes: ModeState,
    pub title: Option<String>,
}

impl Screen {
    pub fn new(rows: u16, cols: u16) -> Screen {
        Screen {
            primary: Grid::new(rows, cols),
            alt: Grid::new(rows, cols),
            active: ScreenKind::Primary,
            scrollback: Vec::new(),
            cursor: Cursor::default(),
            modes: ModeState::default(),
            title: None,
        }
    }

    pub fn active_grid(&self) -> &Grid {
        match self.active {
            ScreenKind::Primary => &self.primary,
            ScreenKind::Alt => &self.alt,
        }
    }

    pub fn active_grid_mut(&mut self) -> &mut Grid {
        match self.active {
            ScreenKind::Primary => &mut self.primary,
            ScreenKind::Alt => &mut self.alt,
        }
    }

    pub fn dims(&self) -> (u16, u16) {
        self.active_grid().dims()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellKind;
    use crate::style::CellStyle;

    #[test]
    fn new_screen() {
        let s = Screen::new(24, 80);
        assert_eq!(s.dims(), (24, 80));
        assert_eq!(s.active, ScreenKind::Primary);
        assert!(s.scrollback.is_empty());
        assert!(s.title.is_none());
    }

    #[test]
    fn active_grid_switches() {
        let mut s = Screen::new(2, 2);
        s.active_grid_mut()
            .set(0, 0, Cell::glyph("p", CellStyle::default()));
        s.active = ScreenKind::Alt;
        assert!(matches!(s.active_grid().cell(0, 0).kind, CellKind::Empty));
        s.active_grid_mut()
            .set(0, 0, Cell::glyph("a", CellStyle::default()));
        assert_eq!(s.active_grid().cell(0, 0).text(), "a");
        s.active = ScreenKind::Primary;
        assert_eq!(s.active_grid().cell(0, 0).text(), "p");
    }

    #[test]
    fn screen_kind_default() {
        assert_eq!(ScreenKind::default(), ScreenKind::Primary);
    }

    #[test]
    fn serde_roundtrip() {
        let s = Screen::new(2, 2);
        let j = serde_json::to_string(&s).unwrap();
        let back: Screen = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }
}
