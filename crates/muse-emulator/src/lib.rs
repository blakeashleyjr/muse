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

/// What the profiled terminal admits to (see [`vt::TermCaps`]).
pub fn term_caps(profile: &Profile) -> vt::TermCaps {
    use muse_core::capabilities::KeyboardProtocol;
    vt::TermCaps {
        kitty_keyboard: profile.caps.keyboard == KeyboardProtocol::Kitty,
        modify_other_keys: matches!(
            profile.caps.keyboard,
            KeyboardProtocol::ModifyOtherKeys | KeyboardProtocol::Kitty
        ),
        sync_output: profile.caps.supports_sync_output,
        bracketed_paste: profile.caps.supports_bracketed_paste,
        mouse: !profile.caps.mouse.is_empty(),
        xtversion: match profile.name.as_str() {
            "xterm" => Some("XTerm(390)".into()),
            "kitty" => Some("kitty(0.36.0)".into()),
            _ => None,
        },
    }
}

impl VtEmulator {
    pub fn new(profile: Profile, cols: u16, rows: u16) -> VtEmulator {
        let mut term = Term::new(
            rows,
            cols,
            profile.caps.da1.clone(),
            profile.caps.da2.clone(),
        );
        term.set_caps(term_caps(&profile));
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
        self.term.set_caps(term_caps(&profile));
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

    #[test]
    fn kitty_profile_negotiates_keyboard_protocol() {
        let mut e = emu("kitty");
        assert_eq!(e.modes().kitty_kbd_flags, 0);
        e.advance(b"\x1b[>1u");
        assert_eq!(e.modes().kitty_kbd_flags, 1);
        e.advance(b"\x1b[?u");
        assert_eq!(e.drain_replies(), b"\x1b[?1u");
        e.advance(b"\x1b[=2;2u"); // set: or-in flag 2
        assert_eq!(e.modes().kitty_kbd_flags, 3);
        e.advance(b"\x1b[>8u");
        assert_eq!(e.modes().kitty_kbd_flags, 8);
        e.advance(b"\x1b[<u");
        assert_eq!(e.modes().kitty_kbd_flags, 3);
        e.advance(b"\x1b[<5u"); // pop more than pushed → empty
        assert_eq!(e.modes().kitty_kbd_flags, 0);
        // the encoder follows
        e.advance(b"\x1b[>1u");
        let bytes = muse_core::input::encode_key(
            &muse_core::input::KeyEvent::new(muse_core::input::Key::Escape),
            e.modes(),
            e.capabilities(),
        );
        assert_eq!(bytes, b"\x1b[27u");
    }

    #[test]
    fn xterm_ignores_kitty_push_but_honours_modify_other_keys() {
        let mut e = emu("xterm");
        e.advance(b"\x1b[>1u\x1b[?u");
        assert_eq!(e.modes().kitty_kbd_flags, 0);
        assert!(e.drain_replies().is_empty(), "no CSI ? u reply on xterm");
        e.advance(b"\x1b[>4;2m");
        assert_eq!(e.modes().modify_other_keys, 2);
        e.advance(b"\x1b[?4m");
        assert_eq!(e.drain_replies(), b"\x1b[>4;2m");
        e.advance(b"\x1b[>4m");
        assert_eq!(e.modes().modify_other_keys, 0);
        // vt220 has neither
        let mut v = emu("vt220");
        v.advance(b"\x1b[>4;2m");
        assert_eq!(v.modes().modify_other_keys, 0);
    }

    #[test]
    fn decrqm_reports_real_state_and_profile_gaps() {
        let mut e = emu("xterm");
        e.advance(b"\x1b[?2026$p");
        assert_eq!(e.drain_replies(), b"\x1b[?2026;2$y", "reset");
        e.advance(b"\x1b[?2026h\x1b[?2026$p");
        assert_eq!(e.drain_replies(), b"\x1b[?2026;1$y", "set");
        assert!(e.modes().sync_output);
        e.advance(b"\x1b[?2004h\x1b[?1006h\x1b[?1002h\x1b[?25l");
        e.advance(b"\x1b[?2004$p\x1b[?1006$p\x1b[?1002$p\x1b[?25$p\x1b[?9999$p\x1b[4$p");
        assert_eq!(
            String::from_utf8_lossy(&e.drain_replies()),
            "\x1b[?2004;1$y\x1b[?1006;1$y\x1b[?1002;1$y\x1b[?25;2$y\x1b[?9999;0$y\x1b[4;0$y"
        );
        // vt220: no sync output, no paste, no mouse — probes say so and
        // setting them is ignored
        let mut v = emu("vt220");
        v.advance(b"\x1b[?2026h\x1b[?2004h\x1b[?1000h");
        assert!(!v.modes().sync_output);
        assert!(!v.modes().bracketed_paste);
        assert_eq!(v.modes().mouse, muse_core::modes::MouseMode::Off);
        v.advance(b"\x1b[?2026$p\x1b[?2004$p\x1b[?1000$p");
        assert_eq!(
            String::from_utf8_lossy(&v.drain_replies()),
            "\x1b[?2026;0$y\x1b[?2004;0$y\x1b[?1000;0$y"
        );
    }

    #[test]
    fn xtversion_per_profile() {
        let mut e = emu("xterm");
        e.advance(b"\x1b[>q");
        assert_eq!(e.drain_replies(), b"\x1bP>|XTerm(390)\x1b\\");
        let mut k = emu("kitty");
        k.advance(b"\x1b[>q");
        assert!(String::from_utf8_lossy(&k.drain_replies()).contains("kitty"));
        let mut v = emu("vt220");
        v.advance(b"\x1b[>q");
        assert!(v.drain_replies().is_empty());
    }

    #[test]
    fn dec_special_graphics_charset() {
        let mut e = emu("xterm");
        e.advance(b"\x1b(0lqqk\x1b(Bx");
        let s = e.snapshot_screen();
        let row: String = (0..5)
            .map(|c| s.primary.cell(0, c).text())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(row, "┌──┐x");
        // G1 via SO/SI
        let mut e = emu("xterm");
        e.advance(b"\x1b)0a\x0eq\x0fq");
        let s = e.snapshot_screen();
        assert_eq!(s.primary.cell(0, 0).text(), "a");
        assert_eq!(s.primary.cell(0, 1).text(), "─");
        assert_eq!(s.primary.cell(0, 2).text(), "q");
        // RIS keeps the profile's caps
        let mut k = emu("kitty");
        k.advance(b"\x1bc\x1b[>1u");
        assert_eq!(k.modes().kitty_kbd_flags, 1);
    }
}
