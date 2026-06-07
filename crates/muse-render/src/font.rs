//! Built-in deterministic bitmap font (baked GNU Unifont subset).
//!
//! Narrow glyphs are 8×16, wide glyphs 16×16. Rows are MSB-aligned u16. The
//! data is static (see `font_data.rs`), so rasterization is byte-identical on
//! every platform — no system fonts, no floating point.

use crate::font_data::GLYPHS;
use std::collections::HashMap;
use std::sync::OnceLock;

pub const CELL_W: u32 = 8;
pub const CELL_H: u32 = 16;

/// A single glyph bitmap.
#[derive(Clone, Copy)]
pub struct Glyph {
    pub width_cells: u8,
    pub rows: [u16; 16],
}

static TABLE: OnceLock<HashMap<u32, Glyph>> = OnceLock::new();

fn table() -> &'static HashMap<u32, Glyph> {
    TABLE.get_or_init(|| {
        let mut m = HashMap::with_capacity(GLYPHS.len());
        for &(cp, w, rows) in GLYPHS {
            m.insert(
                cp,
                Glyph {
                    width_cells: w,
                    rows,
                },
            );
        }
        m
    })
}

/// The "missing glyph" hollow box (drawn for any codepoint not in the table).
pub fn tofu(width_cells: u8) -> Glyph {
    let full: u16 = if width_cells == 2 { 0xFFFF } else { 0xFF00 };
    let edge: u16 = if width_cells == 2 { 0x8001 } else { 0x8100 };
    let mut rows = [0u16; 16];
    for (i, r) in rows.iter_mut().enumerate() {
        *r = if i == 1 || i == 14 {
            full
        } else if (2..14).contains(&i) {
            edge
        } else {
            0
        };
    }
    Glyph { width_cells, rows }
}

/// Look up the glyph for the first scalar of a grapheme cluster.
/// Returns `tofu` for unknown codepoints. `display_width` (1 or 2) decides the
/// tofu box width.
pub fn lookup(grapheme: &str, display_width: u8) -> Glyph {
    let cp = grapheme.chars().next().map(|c| c as u32).unwrap_or(0x20);
    if let Some(g) = table().get(&cp) {
        *g
    } else {
        tofu(display_width.max(1))
    }
}

/// True if a glyph for this codepoint is baked in.
pub fn has_glyph(cp: u32) -> bool {
    table().contains_key(&cp)
}

/// A stable FNV-1a fingerprint of the baked font data. Pinned in tests so the
/// embedded font cannot silently drift (§25).
pub fn fingerprint() -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    };
    for &(cp, w, rows) in GLYPHS {
        for b in cp.to_le_bytes() {
            feed(b);
        }
        feed(w);
        for r in rows {
            for b in r.to_le_bytes() {
                feed(b);
            }
        }
    }
    h
}

/// Number of baked glyphs.
pub fn glyph_count() -> usize {
    GLYPHS.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_present() {
        assert!(has_glyph('A' as u32));
        assert!(has_glyph('0' as u32));
        assert!(has_glyph(' ' as u32));
    }

    #[test]
    fn space_is_blank() {
        let g = lookup(" ", 1);
        assert!(g.rows.iter().all(|&r| r == 0));
        assert_eq!(g.width_cells, 1);
    }

    #[test]
    fn letter_a_has_pixels() {
        let g = lookup("A", 1);
        assert!(g.rows.iter().any(|&r| r != 0));
    }

    #[test]
    fn cjk_is_wide() {
        let g = lookup("日", 2);
        assert_eq!(g.width_cells, 2);
    }

    #[test]
    fn unknown_is_tofu() {
        // a codepoint we did not bake
        let g = lookup("\u{1F600}", 2);
        assert_eq!(g.width_cells, 2);
        assert!(g.rows.iter().any(|&r| r != 0));
        assert!(!has_glyph(0x1F600));
    }

    #[test]
    fn empty_grapheme_defaults_space() {
        let g = lookup("", 1);
        assert!(g.rows.iter().all(|&r| r == 0));
    }

    #[test]
    fn tofu_widths() {
        assert_eq!(tofu(1).width_cells, 1);
        assert_eq!(tofu(2).width_cells, 2);
    }

    #[test]
    fn fingerprint_is_stable() {
        // Pinning the embedded font: if PINNED changes, the font data changed
        // and pixel baselines must be regenerated intentionally (§25).
        const PINNED: u64 = 0xa9eeda1904072b8a;
        let fp = fingerprint();
        assert_eq!(fp, fingerprint());
        assert!(glyph_count() >= 95);
        assert_eq!(
            fp, PINNED,
            "embedded font drifted — regenerate pixel baselines"
        );
    }
}
