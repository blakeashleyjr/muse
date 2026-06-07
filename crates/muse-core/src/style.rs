//! Cell styling: attributes and color triple.

use crate::color::Color;
use serde::{Deserialize, Serialize};

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
    pub struct Attrs: u16 {
        const BOLD = 1;
        const DIM = 2;
        const ITALIC = 4;
        const UNDERLINE = 8;
        const BLINK = 16;
        const REVERSE = 32;
        const HIDDEN = 64;
        const STRIKE = 128;
        const DOUBLE_UNDERLINE = 256;
        const CURLY_UNDERLINE = 512;
    }
}

impl Attrs {
    /// Canonical, stable, sorted list of attribute names that are set.
    /// Used by the styled snapshot/conformance formats.
    pub fn names(&self) -> Vec<&'static str> {
        const ALL: &[(Attrs, &str)] = &[
            (Attrs::BOLD, "BOLD"),
            (Attrs::DIM, "DIM"),
            (Attrs::ITALIC, "ITALIC"),
            (Attrs::UNDERLINE, "UNDERLINE"),
            (Attrs::BLINK, "BLINK"),
            (Attrs::REVERSE, "REVERSE"),
            (Attrs::HIDDEN, "HIDDEN"),
            (Attrs::STRIKE, "STRIKE"),
            (Attrs::DOUBLE_UNDERLINE, "DOUBLE_UNDERLINE"),
            (Attrs::CURLY_UNDERLINE, "CURLY_UNDERLINE"),
        ];
        ALL.iter()
            .filter(|(a, _)| self.contains(*a))
            .map(|(_, n)| *n)
            .collect()
    }

    /// Parse from a canonical name (inverse of the entries in [`Attrs::names`]).
    pub fn parse_name(name: &str) -> Option<Attrs> {
        Some(match name {
            "BOLD" => Attrs::BOLD,
            "DIM" => Attrs::DIM,
            "ITALIC" => Attrs::ITALIC,
            "UNDERLINE" => Attrs::UNDERLINE,
            "BLINK" => Attrs::BLINK,
            "REVERSE" => Attrs::REVERSE,
            "HIDDEN" => Attrs::HIDDEN,
            "STRIKE" => Attrs::STRIKE,
            "DOUBLE_UNDERLINE" => Attrs::DOUBLE_UNDERLINE,
            "CURLY_UNDERLINE" => Attrs::CURLY_UNDERLINE,
            _ => return None,
        })
    }
}

/// The full style of a single cell.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct CellStyle {
    pub fg: Color,
    pub bg: Color,
    pub underline: Color,
    pub attrs: Attrs,
}

impl CellStyle {
    /// Apply the REVERSE attribute by swapping fg/bg, returning the visually
    /// effective style. Useful for rendering.
    pub fn effective(&self) -> CellStyle {
        if self.attrs.contains(Attrs::REVERSE) {
            CellStyle {
                fg: self.bg,
                bg: self.fg,
                underline: self.underline,
                attrs: self.attrs,
            }
        } else {
            *self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sorted_and_filtered() {
        let a = Attrs::BOLD | Attrs::ITALIC | Attrs::STRIKE;
        assert_eq!(a.names(), vec!["BOLD", "ITALIC", "STRIKE"]);
        assert!(Attrs::empty().names().is_empty());
    }

    #[test]
    fn all_names_roundtrip() {
        for a in [
            Attrs::BOLD,
            Attrs::DIM,
            Attrs::ITALIC,
            Attrs::UNDERLINE,
            Attrs::BLINK,
            Attrs::REVERSE,
            Attrs::HIDDEN,
            Attrs::STRIKE,
            Attrs::DOUBLE_UNDERLINE,
            Attrs::CURLY_UNDERLINE,
        ] {
            let name = a.names()[0];
            assert_eq!(Attrs::parse_name(name), Some(a));
        }
        assert_eq!(Attrs::parse_name("NOPE"), None);
    }

    #[test]
    fn effective_swaps_on_reverse() {
        let s = CellStyle {
            fg: Color::Indexed(1),
            bg: Color::Indexed(2),
            underline: Color::Default,
            attrs: Attrs::REVERSE,
        };
        let e = s.effective();
        assert_eq!(e.fg, Color::Indexed(2));
        assert_eq!(e.bg, Color::Indexed(1));
    }

    #[test]
    fn effective_noop_without_reverse() {
        let s = CellStyle {
            fg: Color::Indexed(1),
            bg: Color::Indexed(2),
            ..Default::default()
        };
        assert_eq!(s.effective(), s);
    }

    #[test]
    fn default_style() {
        let s = CellStyle::default();
        assert_eq!(s.fg, Color::Default);
        assert_eq!(s.attrs, Attrs::empty());
    }
}
