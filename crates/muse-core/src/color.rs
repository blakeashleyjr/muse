//! Terminal colors.

use serde::{Deserialize, Serialize};

/// A terminal color. `Default` means "use the terminal's default fg/bg".
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    /// Resolve this color to a concrete 24-bit RGB triple using the standard
    /// xterm 256-color palette. `Default` resolves to `default_rgb`.
    pub fn to_rgb(self, default_rgb: (u8, u8, u8)) -> (u8, u8, u8) {
        match self {
            Color::Default => default_rgb,
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(i) => index_to_rgb(i),
        }
    }
}

/// The 16 base ANSI colors as RGB (xterm defaults).
pub const ANSI16: [(u8, u8, u8); 16] = [
    (0, 0, 0),       // 0 black
    (205, 0, 0),     // 1 red
    (0, 205, 0),     // 2 green
    (205, 205, 0),   // 3 yellow
    (0, 0, 238),     // 4 blue
    (205, 0, 205),   // 5 magenta
    (0, 205, 205),   // 6 cyan
    (229, 229, 229), // 7 white
    (127, 127, 127), // 8 bright black
    (255, 0, 0),     // 9 bright red
    (0, 255, 0),     // 10 bright green
    (255, 255, 0),   // 11 bright yellow
    (92, 92, 255),   // 12 bright blue
    (255, 0, 255),   // 13 bright magenta
    (0, 255, 255),   // 14 bright cyan
    (255, 255, 255), // 15 bright white
];

/// Map an xterm 256-palette index to RGB.
pub fn index_to_rgb(i: u8) -> (u8, u8, u8) {
    match i {
        0..=15 => ANSI16[i as usize],
        16..=231 => {
            let i = i - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let conv = |v: u8| -> u8 {
                if v == 0 {
                    0
                } else {
                    55 + v * 40
                }
            };
            (conv(r), conv(g), conv(b))
        }
        232..=255 => {
            let level = 8 + (i - 232) * 10;
            (level, level, level)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_default() {
        assert_eq!(Color::default(), Color::Default);
    }

    #[test]
    fn default_resolves_to_provided() {
        assert_eq!(Color::Default.to_rgb((1, 2, 3)), (1, 2, 3));
    }

    #[test]
    fn rgb_passthrough() {
        assert_eq!(Color::Rgb(10, 20, 30).to_rgb((0, 0, 0)), (10, 20, 30));
    }

    #[test]
    fn ansi_index_resolves() {
        assert_eq!(Color::Indexed(1).to_rgb((0, 0, 0)), (205, 0, 0));
        assert_eq!(index_to_rgb(0), (0, 0, 0));
        assert_eq!(index_to_rgb(15), (255, 255, 255));
    }

    #[test]
    fn cube_index_resolves() {
        // 16 is the start of the 6x6x6 cube => (0,0,0)
        assert_eq!(index_to_rgb(16), (0, 0, 0));
        // 231 is the last cube color => (255,255,255)
        assert_eq!(index_to_rgb(231), (255, 255, 255));
        // a mid cube color
        assert_eq!(index_to_rgb(16 + 36 + 6 + 1), (95, 95, 95));
    }

    #[test]
    fn grayscale_index_resolves() {
        assert_eq!(index_to_rgb(232), (8, 8, 8));
        assert_eq!(index_to_rgb(255), (238, 238, 238));
    }

    #[test]
    fn serde_roundtrip() {
        for c in [Color::Default, Color::Indexed(5), Color::Rgb(1, 2, 3)] {
            let s = serde_json::to_string(&c).unwrap();
            let back: Color = serde_json::from_str(&s).unwrap();
            assert_eq!(c, back);
        }
    }
}
