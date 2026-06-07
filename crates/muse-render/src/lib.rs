//! `muse-render` — text / styled / pixel renderers (§11).

pub mod font;
mod font_data;
pub mod pixel;
pub mod styled;
pub mod svg;
pub mod text;

use muse_core::screen::Screen;
use muse_core::snapshot::{Snapshot, SnapshotKind};

pub use pixel::rasterize_rgba;

/// A renderer turns a screen + kind into a snapshot.
pub trait Renderer {
    fn render(&self, s: &Screen, k: SnapshotKind) -> Snapshot;
}

/// The built-in renderer (deterministic).
#[derive(Default, Clone, Copy)]
pub struct DefaultRenderer;

impl Renderer for DefaultRenderer {
    fn render(&self, s: &Screen, k: SnapshotKind) -> Snapshot {
        match k {
            SnapshotKind::Text => Snapshot::Text(text::render_text(s)),
            SnapshotKind::Styled => Snapshot::Styled(styled::render_styled(s)),
            SnapshotKind::Pixel { scale } => Snapshot::Pixel(pixel::render_pixel(s, scale)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::cell::Cell;
    use muse_core::style::CellStyle;

    fn screen() -> Screen {
        let mut s = Screen::new(2, 4);
        s.cursor.visible = false;
        s.primary.set(0, 0, Cell::glyph("h", CellStyle::default()));
        s.primary.set(0, 1, Cell::glyph("i", CellStyle::default()));
        s
    }

    #[test]
    fn renders_text() {
        let r = DefaultRenderer;
        assert!(matches!(
            r.render(&screen(), SnapshotKind::Text),
            Snapshot::Text(t) if t == "hi"
        ));
    }

    #[test]
    fn renders_styled() {
        let r = DefaultRenderer;
        assert!(matches!(
            r.render(&screen(), SnapshotKind::Styled),
            Snapshot::Styled(s) if s.rows[0].text == "hi"
        ));
    }

    #[test]
    fn renders_pixel() {
        let r = DefaultRenderer;
        assert!(matches!(
            r.render(&screen(), SnapshotKind::Pixel { scale: 1 }),
            Snapshot::Pixel(p) if p.width == 32
        ));
    }

    #[test]
    fn rasterize_rgba_reexport() {
        let (w, h, buf) = rasterize_rgba(&screen(), 1);
        assert_eq!(w, 32);
        assert_eq!(h, 32);
        assert_eq!(buf.len(), (w * h * 4) as usize);
    }
}
