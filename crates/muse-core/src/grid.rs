//! The grid: rows × cols matrix of cells, the queryable "DOM".

use crate::cell::Cell;
use serde::{Deserialize, Serialize};

/// A rectangular region in grid coordinates. `row`/`col` are the top-left,
/// `w`/`h` the extent in columns/rows. A 1×1 cell rect has w=1, h=1.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Rect {
    pub row: u16,
    pub col: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub fn cell(row: u16, col: u16) -> Rect {
        Rect {
            row,
            col,
            w: 1,
            h: 1,
        }
    }

    pub fn new(row: u16, col: u16, w: u16, h: u16) -> Rect {
        Rect { row, col, w, h }
    }

    /// True if (r, c) is inside this rect.
    pub fn contains(&self, r: u16, c: u16) -> bool {
        r >= self.row
            && c >= self.col
            && r < self.row.saturating_add(self.h)
            && c < self.col.saturating_add(self.w)
    }

    /// Iterate over all (row, col) coordinates within the rect.
    pub fn coords(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        (self.row..self.row.saturating_add(self.h))
            .flat_map(move |r| (self.col..self.col.saturating_add(self.w)).map(move |c| (r, c)))
    }
}

/// Row-major grid of cells.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Grid {
    rows: u16,
    cols: u16,
    cells: Vec<Cell>,
}

impl Grid {
    /// A grid of all-empty cells.
    pub fn new(rows: u16, cols: u16) -> Grid {
        Grid {
            rows,
            cols,
            cells: vec![Cell::empty(); rows as usize * cols as usize],
        }
    }

    /// Build a grid from explicit rows of cells (each row padded/truncated to `cols`).
    pub fn from_rows(rows_in: Vec<Vec<Cell>>, cols: u16) -> Grid {
        let rows = rows_in.len() as u16;
        let mut cells = Vec::with_capacity(rows as usize * cols as usize);
        for mut row in rows_in {
            row.resize(cols as usize, Cell::empty());
            row.truncate(cols as usize);
            cells.extend(row);
        }
        Grid { rows, cols, cells }
    }

    pub fn dims(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    fn idx(&self, r: u16, c: u16) -> usize {
        r as usize * self.cols as usize + c as usize
    }

    /// Get a cell. Out-of-bounds coordinates return a static empty cell.
    pub fn cell(&self, r: u16, c: u16) -> &Cell {
        if r >= self.rows || c >= self.cols {
            return &EMPTY_CELL;
        }
        &self.cells[self.idx(r, c)]
    }

    /// Mutable cell access; panics if out of bounds.
    pub fn cell_mut(&mut self, r: u16, c: u16) -> &mut Cell {
        let i = self.idx(r, c);
        &mut self.cells[i]
    }

    pub fn set(&mut self, r: u16, c: u16, cell: Cell) {
        if r < self.rows && c < self.cols {
            let i = self.idx(r, c);
            self.cells[i] = cell;
        }
    }

    /// Logical text of a row: joins glyphs, Empty→space, Spacer→"".
    pub fn row_text(&self, r: u16) -> String {
        let mut s = String::new();
        if r >= self.rows {
            return s;
        }
        for c in 0..self.cols {
            s.push_str(self.cell(r, c).text());
        }
        s
    }

    /// Like [`Grid::row_text`] but with trailing whitespace removed.
    pub fn row_text_trimmed(&self, r: u16) -> String {
        self.row_text(r).trim_end().to_string()
    }

    /// All cells in row-major order.
    pub fn iter(&self) -> impl Iterator<Item = &Cell> {
        self.cells.iter()
    }
}

static EMPTY_CELL: Cell = Cell {
    kind: crate::cell::CellKind::Empty,
    style: crate::style::CellStyle {
        fg: crate::color::Color::Default,
        bg: crate::color::Color::Default,
        underline: crate::color::Color::Default,
        attrs: crate::style::Attrs::empty(),
    },
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellKind;
    use crate::style::CellStyle;

    #[test]
    fn new_grid_is_empty() {
        let g = Grid::new(3, 4);
        assert_eq!(g.dims(), (3, 4));
        assert_eq!(g.rows(), 3);
        assert_eq!(g.cols(), 4);
        assert_eq!(g.row_text(0), "    ");
        assert!(matches!(g.cell(0, 0).kind, CellKind::Empty));
    }

    #[test]
    fn set_and_get() {
        let mut g = Grid::new(2, 2);
        g.set(0, 1, Cell::glyph("z", CellStyle::default()));
        assert_eq!(g.cell(0, 1).text(), "z");
        assert_eq!(g.row_text(0), " z");
    }

    #[test]
    fn set_out_of_bounds_ignored() {
        let mut g = Grid::new(1, 1);
        g.set(5, 5, Cell::glyph("z", CellStyle::default()));
        assert_eq!(g.cell(0, 0).text(), " ");
    }

    #[test]
    fn cell_out_of_bounds_is_empty() {
        let g = Grid::new(1, 1);
        assert!(matches!(g.cell(9, 9).kind, CellKind::Empty));
    }

    #[test]
    fn row_text_out_of_bounds_empty() {
        let g = Grid::new(1, 1);
        assert_eq!(g.row_text(5), "");
    }

    #[test]
    fn row_text_trimmed() {
        let mut g = Grid::new(1, 5);
        g.set(0, 0, Cell::glyph("h", CellStyle::default()));
        g.set(0, 1, Cell::glyph("i", CellStyle::default()));
        assert_eq!(g.row_text(0), "hi   ");
        assert_eq!(g.row_text_trimmed(0), "hi");
    }

    #[test]
    fn wide_glyph_row_text() {
        let mut g = Grid::new(1, 3);
        g.set(0, 0, Cell::glyph("日", CellStyle::default()));
        g.set(
            0,
            1,
            Cell {
                kind: CellKind::Spacer,
                style: CellStyle::default(),
            },
        );
        g.set(0, 2, Cell::glyph("x", CellStyle::default()));
        assert_eq!(g.row_text(0), "日x");
    }

    #[test]
    fn from_rows_pads_and_truncates() {
        let g = Grid::from_rows(
            vec![
                vec![Cell::glyph("a", CellStyle::default())],
                vec![
                    Cell::glyph("b", CellStyle::default()),
                    Cell::glyph("c", CellStyle::default()),
                    Cell::glyph("d", CellStyle::default()),
                ],
            ],
            2,
        );
        assert_eq!(g.dims(), (2, 2));
        assert_eq!(g.row_text(0), "a ");
        assert_eq!(g.row_text(1), "bc");
    }

    #[test]
    fn cell_mut_edits() {
        let mut g = Grid::new(1, 1);
        *g.cell_mut(0, 0) = Cell::glyph("q", CellStyle::default());
        assert_eq!(g.cell(0, 0).text(), "q");
    }

    #[test]
    fn iter_counts_all() {
        let g = Grid::new(2, 3);
        assert_eq!(g.iter().count(), 6);
    }

    #[test]
    fn rect_contains_and_coords() {
        let r = Rect::new(1, 1, 2, 2);
        assert!(r.contains(1, 1));
        assert!(r.contains(2, 2));
        assert!(!r.contains(0, 0));
        assert!(!r.contains(3, 1));
        let coords: Vec<_> = r.coords().collect();
        assert_eq!(coords, vec![(1, 1), (1, 2), (2, 1), (2, 2)]);
    }

    #[test]
    fn rect_cell_helper() {
        let r = Rect::cell(4, 5);
        assert_eq!((r.row, r.col, r.w, r.h), (4, 5, 1, 1));
    }
}
