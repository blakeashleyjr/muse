//! Text tier (§11): trimmed golden text of the active grid.

use muse_core::screen::Screen;

/// Render the active grid to golden text: trailing spaces trimmed per line,
/// trailing blank lines stripped.
pub fn render_text(screen: &Screen) -> String {
    let grid = screen.active_grid();
    let (rows, _) = grid.dims();
    let mut lines: Vec<String> = (0..rows).map(|r| grid.row_text_trimmed(r)).collect();
    while matches!(lines.last(), Some(l) if l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::cell::Cell;
    use muse_core::style::CellStyle;

    fn screen_with(lines: &[&str]) -> Screen {
        let mut s = Screen::new(5, 20);
        for (r, line) in lines.iter().enumerate() {
            for (c, ch) in line.chars().enumerate() {
                s.primary.set(
                    r as u16,
                    c as u16,
                    Cell::glyph(ch.to_string(), CellStyle::default()),
                );
            }
        }
        s
    }

    #[test]
    fn trims_trailing_blank_lines_and_spaces() {
        let s = screen_with(&["hello   ", "world"]);
        assert_eq!(render_text(&s), "hello\nworld");
    }

    #[test]
    fn empty_screen() {
        let s = Screen::new(3, 5);
        assert_eq!(render_text(&s), "");
    }

    #[test]
    fn preserves_interior_blank_lines() {
        let s = screen_with(&["a", "", "b"]);
        assert_eq!(render_text(&s), "a\n\nb");
    }
}
