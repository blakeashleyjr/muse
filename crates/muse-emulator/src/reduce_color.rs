//! Color reduction (§6): TrueColor→256→16, NoColor drop.

use muse_core::color::{index_to_rgb, Color, ANSI16};
use muse_core::ColorDepth;

/// Nearest xterm-256 palette index for an RGB triple (6×6×6 cube + grayscale).
pub fn truecolor_to_256(r: u8, g: u8, b: u8) -> u8 {
    // Candidate from the color cube.
    let to_cube = |v: u8| -> u8 {
        // cube levels: 0,95,135,175,215,255
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let mut best = 0u8;
        let mut bd = u16::MAX;
        for (i, &l) in LEVELS.iter().enumerate() {
            let d = (l as i16 - v as i16).unsigned_abs();
            if d < bd {
                bd = d;
                best = i as u8;
            }
        }
        best
    };
    let (ri, gi, bi) = (to_cube(r), to_cube(g), to_cube(b));
    let cube_idx = 16 + 36 * ri + 6 * gi + bi;
    let (cr, cg, cb) = index_to_rgb(cube_idx);
    let cube_dist = dist((r, g, b), (cr, cg, cb));

    // Candidate from grayscale ramp (232..=255).
    let gray = ((r as u16 + g as u16 + b as u16) / 3) as u8;
    let gidx = if gray < 8 {
        232
    } else if gray > 238 {
        255
    } else {
        232 + ((gray as u16 - 8 + 5) / 10).min(23) as u8
    };
    let (gr, gg, gb) = index_to_rgb(gidx);
    let gray_dist = dist((r, g, b), (gr, gg, gb));

    if gray_dist < cube_dist {
        gidx
    } else {
        cube_idx
    }
}

fn dist(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| {
        let v = x as i32 - y as i32;
        (v * v) as u32
    };
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// Reduce an xterm-256 index to the nearest of the 16 base ANSI colors.
pub fn index256_to_16(i: u8) -> u8 {
    if i < 16 {
        return i;
    }
    let rgb = index_to_rgb(i);
    let mut best = 0u8;
    let mut bd = u32::MAX;
    for (idx, &c) in ANSI16.iter().enumerate() {
        let d = dist(rgb, c);
        if d < bd {
            bd = d;
            best = idx as u8;
        }
    }
    best
}

/// Reduce a single color for the given depth.
pub fn reduce(color: Color, depth: ColorDepth) -> Color {
    match depth {
        ColorDepth::TrueColor => color,
        ColorDepth::Indexed256 => match color {
            Color::Rgb(r, g, b) => Color::Indexed(truecolor_to_256(r, g, b)),
            other => other,
        },
        ColorDepth::Ansi16 => match color {
            Color::Rgb(r, g, b) => Color::Indexed(index256_to_16(truecolor_to_256(r, g, b))),
            Color::Indexed(i) => Color::Indexed(index256_to_16(i)),
            Color::Default => Color::Default,
        },
        ColorDepth::NoColor => Color::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_red_to_256() {
        // (255,0,0) is ANSI bright red index 9 / cube 196
        let i = truecolor_to_256(255, 0, 0);
        assert_eq!(index_to_rgb(i), (255, 0, 0));
    }

    #[test]
    fn black_white_256() {
        assert_eq!(index_to_rgb(truecolor_to_256(0, 0, 0)), (0, 0, 0));
        assert_eq!(
            index_to_rgb(truecolor_to_256(255, 255, 255)),
            (255, 255, 255)
        );
    }

    #[test]
    fn gray_picks_ramp() {
        // mid gray ~ (128,128,128) should map near grayscale ramp
        let i = truecolor_to_256(128, 128, 128);
        let (r, g, b) = index_to_rgb(i);
        assert!(r == g && g == b, "expected gray, got {r},{g},{b}");
    }

    #[test]
    fn index_to_16_keeps_low() {
        assert_eq!(index256_to_16(5), 5);
    }

    #[test]
    fn index_to_16_reduces_high() {
        // 196 = pure red cube => nearest ansi16 is 9 (bright red) or 1
        let v = index256_to_16(196);
        assert!(v == 1 || v == 9);
    }

    #[test]
    fn reduce_truecolor_noop() {
        assert_eq!(
            reduce(Color::Rgb(1, 2, 3), ColorDepth::TrueColor),
            Color::Rgb(1, 2, 3)
        );
    }

    #[test]
    fn reduce_to_256() {
        assert!(matches!(
            reduce(Color::Rgb(255, 0, 0), ColorDepth::Indexed256),
            Color::Indexed(_)
        ));
        // index passthrough
        assert_eq!(
            reduce(Color::Indexed(5), ColorDepth::Indexed256),
            Color::Indexed(5)
        );
    }

    #[test]
    fn reduce_to_16() {
        assert!(matches!(
            reduce(Color::Rgb(255, 0, 0), ColorDepth::Ansi16),
            Color::Indexed(i) if i < 16
        ));
        assert!(matches!(
            reduce(Color::Indexed(200), ColorDepth::Ansi16),
            Color::Indexed(i) if i < 16
        ));
        assert_eq!(reduce(Color::Default, ColorDepth::Ansi16), Color::Default);
    }

    #[test]
    fn reduce_nocolor_drops() {
        assert_eq!(
            reduce(Color::Rgb(1, 2, 3), ColorDepth::NoColor),
            Color::Default
        );
        assert_eq!(
            reduce(Color::Indexed(1), ColorDepth::NoColor),
            Color::Default
        );
    }
}
