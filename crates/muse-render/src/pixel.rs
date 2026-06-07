//! Pixel tier (§11): deterministic rasterization to RGBA PNG.
//!
//! Fixed cell metrics (8×16 × scale), fixed 16-color palette, built-in bitmap
//! font, no AA, no system fonts. Identical `Screen` ⇒ byte-identical PNG.

use crate::font::{self, CELL_H, CELL_W};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};
use muse_core::cell::CellKind;
use muse_core::screen::Screen;
use muse_core::snapshot::PixelSnapshot;
use muse_core::style::Attrs;

const DEFAULT_FG: (u8, u8, u8) = (229, 229, 229); // ANSI 7
const DEFAULT_BG: (u8, u8, u8) = (0, 0, 0); // ANSI 0

fn rgba(c: (u8, u8, u8)) -> [u8; 4] {
    [c.0, c.1, c.2, 255]
}

/// Render the active grid to a deterministic RGBA pixel buffer (before PNG).
fn rasterize(screen: &Screen, scale: u8) -> (u32, u32, Vec<u8>) {
    let scale = scale.max(1) as u32;
    let grid = screen.active_grid();
    let (rows, cols) = grid.dims();
    let base_w = cols as u32 * CELL_W;
    let base_h = rows as u32 * CELL_H;
    let w = base_w * scale;
    let h = base_h * scale;
    let mut buf = vec![0u8; (w * h * 4) as usize];

    let put = |buf: &mut [u8], x: u32, y: u32, px: [u8; 4]| {
        // fill a scale×scale block at logical (x,y)
        for dy in 0..scale {
            for dx in 0..scale {
                let rx = x * scale + dx;
                let ry = y * scale + dy;
                let idx = ((ry * w) + rx) as usize * 4;
                buf[idx..idx + 4].copy_from_slice(&px);
            }
        }
    };

    for r in 0..rows {
        for c in 0..cols {
            let cell = grid.cell(r, c);
            if matches!(cell.kind, CellKind::Spacer) {
                continue; // covered by the wide glyph to the left
            }
            let st = cell.style.effective();
            let fg = rgba(st.fg.to_rgb(DEFAULT_FG));
            let bg = rgba(st.bg.to_rgb(DEFAULT_BG));
            let cell_w_cells = cell.width().max(1) as u32;
            // paint background across the cell(s)
            let x0 = c as u32 * CELL_W;
            let y0 = r as u32 * CELL_H;
            for dy in 0..CELL_H {
                for dx in 0..(CELL_W * cell_w_cells) {
                    put(&mut buf, x0 + dx, y0 + dy, bg);
                }
            }
            // paint glyph
            if !st.attrs.contains(Attrs::HIDDEN) {
                if let CellKind::Glyph(g) = &cell.kind {
                    let glyph = font::lookup(g, cell.width().max(1));
                    let gw = if glyph.width_cells == 2 { 16 } else { 8 };
                    for (ry, &bits) in glyph.rows.iter().enumerate() {
                        for bx in 0..gw {
                            let on = (bits >> (15 - bx)) & 1 == 1;
                            if on {
                                put(&mut buf, x0 + bx as u32, y0 + ry as u32, fg);
                            }
                        }
                    }
                }
                // underline
                if st
                    .attrs
                    .intersects(Attrs::UNDERLINE | Attrs::DOUBLE_UNDERLINE | Attrs::CURLY_UNDERLINE)
                {
                    let uy = 14;
                    for dx in 0..(CELL_W * cell_w_cells) {
                        put(&mut buf, x0 + dx, y0 + uy, fg);
                    }
                }
                // strikethrough
                if st.attrs.contains(Attrs::STRIKE) {
                    let sy = 8;
                    for dx in 0..(CELL_W * cell_w_cells) {
                        put(&mut buf, x0 + dx, y0 + sy, fg);
                    }
                }
            }
        }
    }

    // draw cursor (block) if visible
    if screen.cursor.visible {
        let cr = screen.cursor.row;
        let cc = screen.cursor.col;
        if cr < rows && cc < cols {
            let x0 = cc as u32 * CELL_W;
            let y0 = cr as u32 * CELL_H;
            // invert a thin bar at bottom for determinism
            let cur = rgba(DEFAULT_FG);
            for dy in 13..CELL_H {
                for dx in 0..CELL_W {
                    put(&mut buf, x0 + dx, y0 + dy, cur);
                }
            }
        }
    }

    (w, h, buf)
}

/// Render a deterministic PNG snapshot.
pub fn render_pixel(screen: &Screen, scale: u8) -> PixelSnapshot {
    let (w, h, buf) = rasterize(screen, scale);
    let mut png = Vec::new();
    let enc = PngEncoder::new_with_quality(&mut png, CompressionType::Best, FilterType::NoFilter);
    enc.write_image(&buf, w, h, ExtendedColorType::Rgba8)
        .expect("png encode");
    PixelSnapshot {
        width: w,
        height: h,
        png,
    }
}

/// Raw RGBA buffer accessor (used by the diff subsystem).
pub fn rasterize_rgba(screen: &Screen, scale: u8) -> (u32, u32, Vec<u8>) {
    rasterize(screen, scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::cell::Cell;
    use muse_core::style::CellStyle;

    fn screen() -> Screen {
        let mut s = Screen::new(2, 4);
        s.cursor.visible = false;
        s.primary.set(0, 0, Cell::glyph("A", CellStyle::default()));
        s
    }

    #[test]
    fn dimensions() {
        let snap = render_pixel(&screen(), 1);
        assert_eq!(snap.width, 4 * 8);
        assert_eq!(snap.height, 2 * 16);
        assert!(!snap.png.is_empty());
    }

    #[test]
    fn scale_multiplies() {
        let snap = render_pixel(&screen(), 2);
        assert_eq!(snap.width, 4 * 8 * 2);
        assert_eq!(snap.height, 2 * 16 * 2);
    }

    #[test]
    fn deterministic_same_bytes() {
        let a = render_pixel(&screen(), 1);
        let b = render_pixel(&screen(), 1);
        assert_eq!(a.png, b.png);
    }

    #[test]
    fn content_changes_pixels() {
        let mut s2 = screen();
        s2.primary.set(0, 0, Cell::glyph("B", CellStyle::default()));
        let a = rasterize_rgba(&screen(), 1);
        let b = rasterize_rgba(&s2, 1);
        assert_ne!(a.2, b.2);
    }

    #[test]
    fn cursor_drawn() {
        let mut s = Screen::new(1, 2);
        s.cursor.visible = true;
        let with_cursor = rasterize_rgba(&s, 1);
        s.cursor.visible = false;
        let without = rasterize_rgba(&s, 1);
        assert_ne!(with_cursor.2, without.2);
    }

    #[test]
    fn wide_glyph_spans_two_cells() {
        let mut s = Screen::new(1, 4);
        s.cursor.visible = false;
        s.primary.set(0, 0, Cell::glyph("日", CellStyle::default()));
        s.primary.set(
            0,
            1,
            Cell {
                kind: CellKind::Spacer,
                style: CellStyle::default(),
            },
        );
        let (w, _, _) = rasterize_rgba(&s, 1);
        assert_eq!(w, 32);
    }

    #[test]
    fn scale_zero_treated_as_one() {
        let snap = render_pixel(&screen(), 0);
        assert_eq!(snap.width, 4 * 8);
    }

    fn styled_cell(ch: &str, attrs: muse_core::style::Attrs) -> Screen {
        let mut s = Screen::new(1, 4);
        s.cursor.visible = false;
        let st = CellStyle {
            fg: muse_core::color::Color::Indexed(2),
            attrs,
            ..Default::default()
        };
        s.primary.set(0, 0, Cell::glyph(ch, st));
        s
    }

    #[test]
    fn hidden_attr_skips_glyph() {
        use muse_core::style::Attrs;
        let shown = rasterize_rgba(&styled_cell("A", Attrs::empty()), 1);
        let hidden = rasterize_rgba(&styled_cell("A", Attrs::HIDDEN), 1);
        assert_ne!(shown.2, hidden.2);
    }

    #[test]
    fn underline_and_strike_draw() {
        use muse_core::style::Attrs;
        let plain = rasterize_rgba(&styled_cell(" ", Attrs::empty()), 1);
        let underline = rasterize_rgba(&styled_cell(" ", Attrs::UNDERLINE), 1);
        let strike = rasterize_rgba(&styled_cell(" ", Attrs::STRIKE), 1);
        assert_ne!(plain.2, underline.2);
        assert_ne!(plain.2, strike.2);
    }

    #[test]
    fn reverse_swaps_colors() {
        use muse_core::style::Attrs;
        let normal = rasterize_rgba(&styled_cell("X", Attrs::empty()), 1);
        let reversed = rasterize_rgba(&styled_cell("X", Attrs::REVERSE), 1);
        assert_ne!(normal.2, reversed.2);
    }

    #[test]
    fn cursor_out_of_bounds_not_drawn() {
        let mut s = Screen::new(1, 2);
        s.cursor.visible = true;
        s.cursor.row = 50;
        s.cursor.col = 50;
        let with = rasterize_rgba(&s, 1);
        s.cursor.visible = false;
        let without = rasterize_rgba(&s, 1);
        assert_eq!(with.2, without.2);
    }
}
