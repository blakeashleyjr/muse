//! `muse-core` — pure domain types, traits, and algorithms for the muse
//! terminal-testing system. No I/O, no async, no protobuf.
//!
//! Layering rule: every other crate may depend on this one; this one depends on
//! nothing in the workspace.

pub mod capabilities;
pub mod cell;
pub mod color;
pub mod config;
pub mod cursor;
pub mod error;
pub mod grid;
pub mod input;
pub mod locator;
pub mod modes;
pub mod screen;
pub mod snapshot;
pub mod style;

pub use capabilities::{Capabilities, ColorDepth, KeyboardProtocol, Profile, WidthMode};
pub use cell::{Cell, CellKind};
pub use color::Color;
pub use config::Config;
pub use cursor::{Cursor, CursorShape};
pub use error::{Error, FfiStatus, GrpcCode, Result};
pub use grid::{Grid, Rect};
pub use input::{
    encode_key, encode_mouse, encode_paste, Key, KeyEvent, Mods, MouseAction, MouseButton,
    MouseEvent,
};
pub use locator::{resolve, Locator, Match, StylePredicate};
pub use modes::{ModeState, MouseEnc, MouseMode};
pub use screen::{Screen, ScreenKind};
pub use snapshot::{Snapshot, SnapshotKind, StyleRun, StyledRow, StyledSnapshot};
pub use style::{Attrs, CellStyle};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexports_usable() {
        let s = Screen::new(2, 2);
        let _ = resolve(&s, &Locator::Cursor, false);
        let c: Color = Color::Default;
        assert_eq!(c, Color::Default);
    }
}
