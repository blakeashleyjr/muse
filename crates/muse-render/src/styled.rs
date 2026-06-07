//! Styled tier (§11): diff-friendly text + run-length style list.

use muse_core::screen::Screen;
use muse_core::snapshot::{StyleRun, StyledRow, StyledSnapshot};
use muse_core::style::CellStyle;

fn row_content_width(grid: &muse_core::grid::Grid, r: u16) -> u16 {
    let (_, cols) = grid.dims();
    let mut last = 0u16;
    for c in 0..cols {
        let cell = grid.cell(r, c);
        if !cell.is_blank() || cell.style != CellStyle::default() {
            last = c + 1;
        }
    }
    last
}

/// Build the styled snapshot of the active grid.
pub fn render_styled(screen: &Screen) -> StyledSnapshot {
    let grid = screen.active_grid();
    let (rows, _) = grid.dims();
    let mut out = StyledSnapshot::default();
    for r in 0..rows {
        let width = row_content_width(grid, r);
        let mut runs = Vec::new();
        let mut c = 0u16;
        while c < width {
            let style = grid.cell(r, c).style;
            let start = c;
            while c < width && grid.cell(r, c).style == style {
                c += 1;
            }
            runs.push(StyleRun {
                start_col: start,
                len: c - start,
                fg: style.fg,
                bg: style.bg,
                attrs: style.attrs,
            });
        }
        out.rows.push(StyledRow {
            text: grid.row_text_trimmed(r),
            runs,
        });
    }
    // Drop trailing fully-blank rows (no runs, empty text).
    while matches!(out.rows.last(), Some(row) if row.text.is_empty() && row.runs.is_empty()) {
        out.rows.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::cell::Cell;
    use muse_core::color::Color;
    use muse_core::style::Attrs;

    #[test]
    fn runs_group_by_style() {
        let mut s = Screen::new(2, 10);
        let red = CellStyle {
            fg: Color::Indexed(1),
            attrs: Attrs::BOLD,
            ..Default::default()
        };
        s.primary.set(0, 0, Cell::glyph("H", red));
        s.primary.set(0, 1, Cell::glyph("I", red));
        let snap = render_styled(&s);
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.rows[0].text, "HI");
        assert_eq!(snap.rows[0].runs.len(), 1);
        let run = &snap.rows[0].runs[0];
        assert_eq!(run.start_col, 0);
        assert_eq!(run.len, 2);
        assert_eq!(run.fg, Color::Indexed(1));
        assert!(run.attrs.contains(Attrs::BOLD));
    }

    #[test]
    fn splits_runs_on_style_change() {
        let mut s = Screen::new(1, 10);
        let a = CellStyle {
            fg: Color::Indexed(1),
            ..Default::default()
        };
        let b = CellStyle {
            fg: Color::Indexed(2),
            ..Default::default()
        };
        s.primary.set(0, 0, Cell::glyph("a", a));
        s.primary.set(0, 1, Cell::glyph("b", b));
        let snap = render_styled(&s);
        assert_eq!(snap.rows[0].runs.len(), 2);
    }

    #[test]
    fn empty_screen_no_rows() {
        let s = Screen::new(3, 5);
        assert!(render_styled(&s).rows.is_empty());
    }

    #[test]
    fn canonical_serialization_stable() {
        let mut s = Screen::new(1, 5);
        let red = CellStyle {
            fg: Color::Indexed(1),
            ..Default::default()
        };
        s.primary.set(0, 0, Cell::glyph("X", red));
        let a = render_styled(&s).to_canonical();
        let b = render_styled(&s).to_canonical();
        assert_eq!(a, b);
        assert!(a.contains("fg=idx01"));
    }
}
