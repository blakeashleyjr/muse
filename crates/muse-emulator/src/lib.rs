//! `muse-emulator` — the [`Emulator`] trait, a `vte`-based backend, built-in
//! profiles, and color reduction (§6).

pub mod profile;
pub mod reduce_color;
mod vt;

use muse_core::capabilities::Capabilities;
use muse_core::modes::ModeState;
use muse_core::screen::Screen;
use muse_core::style::CellStyle;
use muse_core::Profile;
use vt::Term;
use vte::Parser;

/// The emulator interface (§6).
pub trait Emulator: Send {
    /// Feed SUT output bytes.
    fn advance(&mut self, bytes: &[u8]);
    /// Build a domain `Screen` from the backend grid (color-reduced per profile).
    fn snapshot_screen(&self) -> Screen;
    fn capabilities(&self) -> &Capabilities;
    /// Bytes to write BACK to the pty (DA/DSR/DECRQM), profile-rewritten.
    fn drain_replies(&mut self) -> Vec<u8>;
    fn resize(&mut self, cols: u16, rows: u16);
    /// Negotiated mode state, kept in sync as bytes are parsed.
    fn modes(&self) -> &ModeState;
    /// Take the count of cooperative `muse:ready` markers since last call.
    fn take_ready(&mut self) -> u32;
    /// Replace the active profile (capabilities, query responses, color depth).
    fn set_profile_dyn(&mut self, profile: Profile);
}

/// The `vte`-backed emulator with a profile shaping layer.
pub struct VtEmulator {
    parser: Parser,
    term: Term,
    profile: Profile,
}

impl VtEmulator {
    pub fn new(profile: Profile, cols: u16, rows: u16) -> VtEmulator {
        let term = Term::new(
            rows,
            cols,
            profile.caps.da1.clone(),
            profile.caps.da2.clone(),
        );
        VtEmulator {
            parser: Parser::new(),
            term,
            profile,
        }
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Replace the profile (and its query responses / color depth).
    pub fn set_profile(&mut self, profile: Profile) {
        self.term.replies.clear();
        self.term
            .set_queries(profile.caps.da1.clone(), profile.caps.da2.clone());
        self.profile = profile;
    }

    fn reduce_style(&self, mut st: CellStyle) -> CellStyle {
        let depth = self.profile.caps.color;
        st.fg = reduce_color::reduce(st.fg, depth);
        st.bg = reduce_color::reduce(st.bg, depth);
        st.underline = reduce_color::reduce(st.underline, depth);
        st
    }
}

impl Emulator for VtEmulator {
    fn advance(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.parser.advance(&mut self.term, b);
        }
    }

    fn snapshot_screen(&self) -> Screen {
        let mut screen = self.term.screen.clone();
        if self.profile.caps.color != muse_core::ColorDepth::TrueColor {
            for grid in [&mut screen.primary, &mut screen.alt] {
                let (rows, cols) = grid.dims();
                for r in 0..rows {
                    for c in 0..cols {
                        let st = self.reduce_style(grid.cell(r, c).style);
                        grid.cell_mut(r, c).style = st;
                    }
                }
            }
            for row in &mut screen.scrollback {
                for cell in row.iter_mut() {
                    cell.style = self.reduce_style(cell.style);
                }
            }
        }
        screen
    }

    fn capabilities(&self) -> &Capabilities {
        &self.profile.caps
    }

    fn drain_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.term.replies)
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(cols, rows);
    }

    fn modes(&self) -> &ModeState {
        &self.term.screen.modes
    }

    fn take_ready(&mut self) -> u32 {
        std::mem::take(&mut self.term.ready_pulses)
    }

    fn set_profile_dyn(&mut self, profile: Profile) {
        self.set_profile(profile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::color::Color;

    fn emu(profile: &str) -> VtEmulator {
        VtEmulator::new(profile::by_name(profile).unwrap(), 10, 5)
    }

    #[test]
    fn xterm_keeps_truecolor() {
        let mut e = emu("xterm");
        e.advance(b"\x1b[38;2;10;20;30mX");
        let s = e.snapshot_screen();
        assert_eq!(s.primary.cell(0, 0).style.fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn acceptance_hello_red_xterm() {
        let mut e = emu("xterm");
        e.advance(b"\x1b[31mHELLO\x1b[0m");
        let s = e.snapshot_screen();
        for c in 0..5 {
            assert_eq!(s.primary.cell(0, c).style.fg, Color::Indexed(1));
        }
        assert_eq!(s.primary.row_text_trimmed(0), "HELLO");
    }

    #[test]
    fn acceptance_hello_dumb_drops_color() {
        let mut e = emu("dumb");
        e.advance(b"\x1b[31mHELLO\x1b[0m");
        let s = e.snapshot_screen();
        assert_eq!(s.primary.cell(0, 0).style.fg, Color::Default);
        assert_eq!(s.primary.row_text_trimmed(0), "HELLO");
    }

    #[test]
    fn acceptance_da1_reply() {
        let mut e = emu("vt220");
        e.advance(b"\x1b[c");
        assert_eq!(e.drain_replies(), b"\x1b[?62;1;2;6;8;9c");
        assert!(e.drain_replies().is_empty());
    }

    #[test]
    fn screen_reduces_truecolor_to_256() {
        let mut e = emu("screen");
        e.advance(b"\x1b[38;2;255;0;0mX");
        let s = e.snapshot_screen();
        assert!(matches!(s.primary.cell(0, 0).style.fg, Color::Indexed(_)));
    }

    #[test]
    fn vt220_reduces_to_16() {
        let mut e = emu("vt220");
        e.advance(b"\x1b[38;5;200mX");
        let s = e.snapshot_screen();
        assert!(matches!(s.primary.cell(0, 0).style.fg, Color::Indexed(i) if i < 16));
    }

    #[test]
    fn modes_tracked() {
        let mut e = emu("xterm");
        e.advance(b"\x1b[?2004h");
        assert!(e.modes().bracketed_paste);
    }

    #[test]
    fn capabilities_exposed() {
        let e = emu("xterm");
        assert_eq!(e.capabilities().terminfo_name, "xterm-256color");
    }

    #[test]
    fn resize_works() {
        let mut e = emu("xterm");
        e.advance(b"hi");
        e.resize(20, 8);
        assert_eq!(e.snapshot_screen().primary.dims(), (8, 20));
    }

    #[test]
    fn take_ready_pulse() {
        let mut e = emu("xterm");
        e.advance(b"\x1b]5379;muse:ready\x07");
        assert_eq!(e.take_ready(), 1);
        assert_eq!(e.take_ready(), 0);
    }

    #[test]
    fn set_profile_swaps_caps() {
        let mut e = emu("xterm");
        e.set_profile(profile::vt220());
        assert_eq!(e.capabilities().color, muse_core::ColorDepth::Ansi16);
        e.advance(b"\x1b[c");
        assert_eq!(e.drain_replies(), b"\x1b[?62;1;2;6;8;9c");
    }

    #[test]
    fn profile_accessor() {
        let e = emu("kitty");
        assert_eq!(e.profile().name, "kitty");
    }
}
