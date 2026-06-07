//! SVG renderer for human review (§11): cells as <rect> + <text>.

use muse_core::cell::CellKind;
use muse_core::color::Color;
use muse_core::screen::Screen;
use muse_core::style::Attrs;

const CW: u32 = 8;
const CH: u32 = 16;
const DEFAULT_FG: (u8, u8, u8) = (229, 229, 229);
const DEFAULT_BG: (u8, u8, u8) = (0, 0, 0);

fn hex(c: Color, dflt: (u8, u8, u8)) -> String {
    let (r, g, b) = c.to_rgb(dflt);
    format!("#{:02x}{:02x}{:02x}", r, g, b)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Render the active grid to a standalone SVG document.
pub fn render_svg(screen: &Screen) -> String {
    let grid = screen.active_grid();
    let (rows, cols) = grid.dims();
    let w = cols as u32 * CW;
    let h = rows as u32 * CH;
    let mut s = String::new();
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">\n"
    ));
    s.push_str(&format!(
        "<rect width=\"{w}\" height=\"{h}\" fill=\"{}\"/>\n",
        hex(Color::Default, DEFAULT_BG)
    ));
    for r in 0..rows {
        for c in 0..cols {
            let cell = grid.cell(r, c);
            if matches!(cell.kind, CellKind::Spacer) {
                continue;
            }
            let st = cell.style.effective();
            let x = c as u32 * CW;
            let y = r as u32 * CH;
            let cw = CW * cell.width().max(1) as u32;
            if st.bg != Color::Default {
                s.push_str(&format!(
                    "<rect x=\"{x}\" y=\"{y}\" width=\"{cw}\" height=\"{CH}\" fill=\"{}\"/>\n",
                    hex(st.bg, DEFAULT_BG)
                ));
            }
            if let CellKind::Glyph(g) = &cell.kind {
                if !st.attrs.contains(Attrs::HIDDEN) {
                    let weight = if st.attrs.contains(Attrs::BOLD) {
                        " font-weight=\"bold\""
                    } else {
                        ""
                    };
                    let style_attr = if st.attrs.contains(Attrs::ITALIC) {
                        " font-style=\"italic\""
                    } else {
                        ""
                    };
                    s.push_str(&format!(
                        "<text x=\"{x}\" y=\"{}\" font-family=\"monospace\" font-size=\"{CH}\" fill=\"{}\"{weight}{style_attr}>{}</text>\n",
                        y + CH - 3,
                        hex(st.fg, DEFAULT_FG),
                        escape(g)
                    ));
                }
            }
        }
    }
    s.push_str("</svg>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::cell::Cell;
    use muse_core::style::{Attrs, CellStyle};

    #[test]
    fn produces_svg() {
        let mut s = Screen::new(1, 3);
        s.primary.set(0, 0, Cell::glyph("A", CellStyle::default()));
        let out = render_svg(&s);
        assert!(out.starts_with("<svg"));
        assert!(out.contains("</svg>"));
        assert!(out.contains(">A</text>"));
    }

    #[test]
    fn escapes_special_chars() {
        let mut s = Screen::new(1, 3);
        s.primary.set(0, 0, Cell::glyph("<", CellStyle::default()));
        assert!(render_svg(&s).contains("&lt;"));
    }

    #[test]
    fn bold_italic_attrs() {
        let mut s = Screen::new(1, 2);
        let st = CellStyle {
            attrs: Attrs::BOLD | Attrs::ITALIC,
            ..Default::default()
        };
        s.primary.set(0, 0, Cell::glyph("X", st));
        let out = render_svg(&s);
        assert!(out.contains("font-weight=\"bold\""));
        assert!(out.contains("font-style=\"italic\""));
    }

    #[test]
    fn bg_rect_emitted() {
        let mut s = Screen::new(1, 2);
        let st = CellStyle {
            bg: Color::Indexed(1),
            ..Default::default()
        };
        s.primary.set(0, 0, Cell::glyph("X", st));
        let out = render_svg(&s);
        // one bg rect besides the full-screen one
        assert!(out.matches("<rect").count() >= 2);
    }
}
