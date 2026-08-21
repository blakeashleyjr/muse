//! A focused VT/ANSI state machine built on `vte`, maintaining the domain
//! `Screen`. This is the single backend (§6); profiles shape its env, queries
//! and color depth.

use muse_core::cell::{Cell, CellKind};
use muse_core::color::Color;
use muse_core::cursor::CursorShape;
use muse_core::modes::{MouseEnc, MouseMode};
use muse_core::screen::{Screen, ScreenKind};
use muse_core::style::{Attrs, CellStyle};
use unicode_width::UnicodeWidthChar;
use vte::Params;

const MAX_SCROLLBACK: usize = 1000;

/// Terminal state + parser performer.
/// Which terminal features this emulator admits to having. Mirrors the
/// profile's capabilities so that a program probing (DECRQM, `CSI ? u`) or
/// negotiating (`CSI ? 2026 h`, `CSI > 1 u`) gets the answer the profiled
/// terminal would give — and so a spec run under `vt220` really does catch
/// "this app assumes bracketed paste".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermCaps {
    pub kitty_keyboard: bool,
    pub modify_other_keys: bool,
    pub sync_output: bool,
    pub bracketed_paste: bool,
    pub mouse: bool,
    /// XTVERSION (`CSI > q`) reply, if the profiled terminal answers it.
    pub xtversion: Option<String>,
}

impl Default for TermCaps {
    fn default() -> Self {
        TermCaps {
            kitty_keyboard: false,
            modify_other_keys: true,
            sync_output: true,
            bracketed_paste: true,
            mouse: true,
            xtversion: Some("XTerm(390)".into()),
        }
    }
}

/// G0/G1 character set designation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Charset {
    #[default]
    Ascii,
    /// DEC Special Graphics (`ESC ( 0`): box-drawing in 0x60..=0x7e.
    DecGraphics,
}

fn dec_graphics(c: char) -> char {
    match c {
        '`' => '◆',
        'a' => '▒',
        'b' => '␉',
        'c' => '␌',
        'd' => '␍',
        'e' => '␊',
        'f' => '°',
        'g' => '±',
        'h' => '␤',
        'i' => '␋',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        other => other,
    }
}

pub struct Term {
    pub screen: Screen,
    pen: CellStyle,
    pending_wrap: bool,
    autowrap: bool,
    scroll_top: u16,
    scroll_bot: u16,
    saved: Option<(u16, u16, CellStyle)>,
    saved_alt: Option<(u16, u16, CellStyle)>,
    tab_width: u16,
    /// Bytes to write back to the PTY (DA/DSR replies). Profile rewriting of
    /// DA1/DA2 happens here via the configured responses.
    pub replies: Vec<u8>,
    da1: Vec<u8>,
    da2: Vec<u8>,
    /// Count of cooperative `muse:ready` OSC pulses seen.
    pub ready_pulses: u32,
    pub caps: TermCaps,
    /// Kitty keyboard flag stack (`CSI > flags u` pushes, `CSI < u` pops).
    kitty_stack: Vec<u8>,
    g0: Charset,
    g1: Charset,
    /// SO (0x0e) selected G1; SI (0x0f) back to G0.
    shift_out: bool,
}

impl Term {
    pub fn new(rows: u16, cols: u16, da1: Vec<u8>, da2: Vec<u8>) -> Term {
        Term {
            screen: Screen::new(rows, cols),
            pen: CellStyle::default(),
            pending_wrap: false,
            autowrap: true,
            scroll_top: 0,
            scroll_bot: rows.saturating_sub(1),
            saved: None,
            saved_alt: None,
            tab_width: 8,
            replies: Vec::new(),
            da1,
            da2,
            ready_pulses: 0,
            caps: TermCaps::default(),
            kitty_stack: Vec::new(),
            g0: Charset::Ascii,
            g1: Charset::Ascii,
            shift_out: false,
        }
    }

    pub fn set_caps(&mut self, caps: TermCaps) {
        self.caps = caps;
    }

    fn active_charset(&self) -> Charset {
        if self.shift_out {
            self.g1
        } else {
            self.g0
        }
    }

    fn sync_kitty_flags(&mut self) {
        self.screen.modes.kitty_kbd_flags = self.kitty_stack.last().copied().unwrap_or(0);
    }

    /// `CSI … u` family: kitty keyboard protocol push/pop/set/query.
    fn kitty_keyboard(&mut self, params: &Params, intermediates: &[u8]) {
        if !self.caps.kitty_keyboard {
            return; // a terminal without the protocol ignores these
        }
        match intermediates.first() {
            Some(b'>') => {
                let flags = praw(params, 0, 0) as u8;
                self.kitty_stack.push(flags);
            }
            Some(b'<') => {
                let n = pn(params, 0, 1) as usize;
                for _ in 0..n {
                    self.kitty_stack.pop();
                }
            }
            Some(b'=') => {
                let flags = praw(params, 0, 0) as u8;
                let mode = praw(params, 1, 1);
                let cur = self.kitty_stack.last().copied().unwrap_or(0);
                let new = match mode {
                    2 => cur | flags,
                    3 => cur & !flags,
                    _ => flags,
                };
                match self.kitty_stack.last_mut() {
                    Some(top) => *top = new,
                    None => self.kitty_stack.push(new),
                }
            }
            Some(b'?') => {
                let cur = self.kitty_stack.last().copied().unwrap_or(0);
                self.replies
                    .extend_from_slice(format!("\x1b[?{cur}u").as_bytes());
            }
            _ => {}
        }
        self.sync_kitty_flags();
    }

    /// DECRQM: report the real state of a DEC private mode (1 set, 2 reset),
    /// or 0 for one this terminal doesn't have.
    fn report_dec_mode(&mut self, mode: u16) {
        let m = &self.screen.modes;
        let state = |on: bool| if on { 1 } else { 2 };
        let value = match mode {
            1 => state(m.app_cursor_keys),
            7 => state(self.autowrap),
            25 => state(self.screen.cursor.visible),
            47 | 1047 | 1049 => state(m.alt_screen),
            1000 if self.caps.mouse => state(m.mouse == MouseMode::Normal),
            1002 if self.caps.mouse => state(m.mouse == MouseMode::ButtonEvent),
            1003 if self.caps.mouse => state(m.mouse == MouseMode::AnyEvent),
            1005 if self.caps.mouse => state(m.mouse_encoding == MouseEnc::Utf8),
            1006 if self.caps.mouse => state(m.mouse_encoding == MouseEnc::Sgr),
            1015 if self.caps.mouse => state(m.mouse_encoding == MouseEnc::Urxvt),
            2004 if self.caps.bracketed_paste => state(m.bracketed_paste),
            2026 if self.caps.sync_output => state(m.sync_output),
            _ => 0,
        };
        self.replies
            .extend_from_slice(format!("\x1b[?{mode};{value}$y").as_bytes());
    }

    fn dims(&self) -> (u16, u16) {
        self.screen.active_grid().dims()
    }

    /// Update the DA1/DA2 query responses (used when the profile changes).
    pub fn set_queries(&mut self, da1: Vec<u8>, da2: Vec<u8>) {
        self.da1 = da1;
        self.da2 = da2;
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        // Preserve top-left content by rebuilding grids.
        let resize_grid = |g: &muse_core::grid::Grid| -> muse_core::grid::Grid {
            let (gr, gc) = g.dims();
            let mut ng = muse_core::grid::Grid::new(rows, cols);
            for r in 0..gr.min(rows) {
                for c in 0..gc.min(cols) {
                    ng.set(r, c, g.cell(r, c).clone());
                }
            }
            ng
        };
        self.screen.primary = resize_grid(&self.screen.primary);
        self.screen.alt = resize_grid(&self.screen.alt);
        self.scroll_top = 0;
        self.scroll_bot = rows.saturating_sub(1);
        self.screen.cursor.row = self.screen.cursor.row.min(rows.saturating_sub(1));
        self.screen.cursor.col = self.screen.cursor.col.min(cols.saturating_sub(1));
        self.pending_wrap = false;
    }

    fn cur(&self) -> (u16, u16) {
        (self.screen.cursor.row, self.screen.cursor.col)
    }

    fn set_cur(&mut self, r: u16, c: u16) {
        let (rows, cols) = self.dims();
        self.screen.cursor.row = r.min(rows.saturating_sub(1));
        self.screen.cursor.col = c.min(cols.saturating_sub(1));
    }

    // ---- scrolling -------------------------------------------------------

    fn scroll_up(&mut self, n: u16) {
        let (_, cols) = self.dims();
        let top = self.scroll_top;
        let bot = self.scroll_bot;
        let to_scrollback = self.screen.active == ScreenKind::Primary && top == 0;
        for _ in 0..n {
            // capture the row leaving the top of the region
            if to_scrollback {
                let mut row = Vec::with_capacity(cols as usize);
                for c in 0..cols {
                    row.push(self.screen.active_grid().cell(top, c).clone());
                }
                self.screen.scrollback.push(row);
                if self.screen.scrollback.len() > MAX_SCROLLBACK {
                    self.screen.scrollback.remove(0);
                }
            }
            let grid = self.screen.active_grid_mut();
            for r in top..bot {
                for c in 0..cols {
                    let below = grid.cell(r + 1, c).clone();
                    grid.set(r, c, below);
                }
            }
            for c in 0..cols {
                grid.set(bot, c, Cell::empty());
            }
        }
    }

    fn scroll_down(&mut self, n: u16) {
        let (_, cols) = self.dims();
        let top = self.scroll_top;
        let bot = self.scroll_bot;
        let grid = self.screen.active_grid_mut();
        for _ in 0..n {
            let mut r = bot;
            while r > top {
                for c in 0..cols {
                    let above = grid.cell(r - 1, c).clone();
                    grid.set(r, c, above);
                }
                r -= 1;
            }
            for c in 0..cols {
                grid.set(top, c, Cell::empty());
            }
        }
    }

    fn line_feed(&mut self) {
        let (row, _) = self.cur();
        if row == self.scroll_bot {
            self.scroll_up(1);
        } else {
            let (rows, _) = self.dims();
            self.screen.cursor.row = (row + 1).min(rows.saturating_sub(1));
        }
        self.pending_wrap = false;
    }

    fn reverse_index(&mut self) {
        let (row, _) = self.cur();
        if row == self.scroll_top {
            self.scroll_down(1);
        } else {
            self.screen.cursor.row = row.saturating_sub(1);
        }
    }

    fn carriage_return(&mut self) {
        self.screen.cursor.col = 0;
        self.pending_wrap = false;
    }

    // ---- printing --------------------------------------------------------

    fn attach_combining(&mut self, c: char) {
        let (row, col) = self.cur();
        // walk left from col-1 to find a glyph
        let mut cc = col;
        while cc > 0 {
            cc -= 1;
            let cell = self.screen.active_grid().cell(row, cc).clone();
            match cell.kind {
                CellKind::Glyph(mut s) => {
                    s.push(c);
                    self.screen
                        .active_grid_mut()
                        .set(row, cc, Cell::glyph(s, cell.style));
                    return;
                }
                CellKind::Spacer => continue,
                CellKind::Empty => return,
            }
        }
    }

    fn print_glyph(&mut self, c: char) {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if w == 0 {
            self.attach_combining(c);
            return;
        }
        let (_, cols) = self.dims();
        if self.pending_wrap && self.autowrap {
            self.carriage_return();
            self.line_feed();
        }
        self.pending_wrap = false;
        let (_, mut col) = self.cur();
        if w == 2 && col + 1 >= cols {
            // wide char doesn't fit on this line
            if self.autowrap {
                self.carriage_return();
                self.line_feed();
                col = 0;
            } else {
                return;
            }
        }
        // row may have advanced via wrap above; re-read it
        let row = self.screen.cursor.row;
        let pen = self.pen;
        self.screen
            .active_grid_mut()
            .set(row, col, Cell::glyph(c.to_string(), pen));
        if w == 2 {
            self.screen.active_grid_mut().set(
                row,
                col + 1,
                Cell {
                    kind: CellKind::Spacer,
                    style: pen,
                },
            );
        }
        let new_col = col + w as u16;
        if new_col >= cols {
            if self.autowrap {
                self.pending_wrap = true;
                self.screen.cursor.col = cols.saturating_sub(1);
            } else {
                self.screen.cursor.col = cols.saturating_sub(1);
            }
        } else {
            self.screen.cursor.col = new_col;
        }
    }

    // ---- erase -----------------------------------------------------------

    fn erase_in_line(&mut self, mode: u16) {
        let (_, cols) = self.dims();
        let (row, col) = self.cur();
        let (a, b) = match mode {
            1 => (0, col + 1),
            2 => (0, cols),
            _ => (col, cols),
        };
        let grid = self.screen.active_grid_mut();
        for c in a..b.min(cols) {
            grid.set(row, c, Cell::empty());
        }
    }

    fn erase_in_display(&mut self, mode: u16) {
        let (rows, cols) = self.dims();
        let (row, _) = self.cur();
        match mode {
            1 => {
                // above (incl cursor line up to cursor)
                for r in 0..row {
                    for c in 0..cols {
                        self.screen.active_grid_mut().set(r, c, Cell::empty());
                    }
                }
                self.erase_in_line(1);
            }
            2 | 3 => {
                for r in 0..rows {
                    for c in 0..cols {
                        self.screen.active_grid_mut().set(r, c, Cell::empty());
                    }
                }
                if mode == 3 {
                    self.screen.scrollback.clear();
                }
            }
            _ => {
                // below
                self.erase_in_line(0);
                for r in (row + 1)..rows {
                    for c in 0..cols {
                        self.screen.active_grid_mut().set(r, c, Cell::empty());
                    }
                }
            }
        }
    }

    fn erase_chars(&mut self, n: u16) {
        let (_, cols) = self.dims();
        let (row, col) = self.cur();
        for c in col..(col + n).min(cols) {
            self.screen.active_grid_mut().set(row, c, Cell::empty());
        }
    }

    fn insert_chars(&mut self, n: u16) {
        let (_, cols) = self.dims();
        let (row, col) = self.cur();
        let grid = self.screen.active_grid_mut();
        let mut c = cols;
        while c > col {
            c -= 1;
            if c >= col + n {
                let src = grid.cell(row, c - n).clone();
                grid.set(row, c, src);
            } else {
                grid.set(row, c, Cell::empty());
            }
        }
    }

    fn delete_chars(&mut self, n: u16) {
        let (_, cols) = self.dims();
        let (row, col) = self.cur();
        let grid = self.screen.active_grid_mut();
        for c in col..cols {
            if c + n < cols {
                let src = grid.cell(row, c + n).clone();
                grid.set(row, c, src);
            } else {
                grid.set(row, c, Cell::empty());
            }
        }
    }

    fn insert_lines(&mut self, n: u16) {
        let (row, _) = self.cur();
        if row < self.scroll_top || row > self.scroll_bot {
            return;
        }
        let saved_top = self.scroll_top;
        self.scroll_top = row;
        self.scroll_down(n);
        self.scroll_top = saved_top;
    }

    fn delete_lines(&mut self, n: u16) {
        let (row, _) = self.cur();
        if row < self.scroll_top || row > self.scroll_bot {
            return;
        }
        let saved_top = self.scroll_top;
        self.scroll_top = row;
        self.scroll_up(n);
        self.scroll_top = saved_top;
    }

    // ---- modes -----------------------------------------------------------

    fn set_dec_mode(&mut self, mode: u16, on: bool) {
        match mode {
            1 => self.screen.modes.app_cursor_keys = on,
            7 => self.autowrap = on,
            25 => self.screen.cursor.visible = on,
            1000 if self.caps.mouse => {
                self.screen.modes.mouse = if on {
                    MouseMode::Normal
                } else {
                    MouseMode::Off
                }
            }
            1002 if self.caps.mouse => {
                self.screen.modes.mouse = if on {
                    MouseMode::ButtonEvent
                } else {
                    MouseMode::Off
                }
            }
            1003 if self.caps.mouse => {
                self.screen.modes.mouse = if on {
                    MouseMode::AnyEvent
                } else {
                    MouseMode::Off
                }
            }
            1005 if self.caps.mouse => {
                self.screen.modes.mouse_encoding = if on {
                    MouseEnc::Utf8
                } else {
                    MouseEnc::Default
                }
            }
            1006 if self.caps.mouse => {
                self.screen.modes.mouse_encoding =
                    if on { MouseEnc::Sgr } else { MouseEnc::Default }
            }
            1015 if self.caps.mouse => {
                self.screen.modes.mouse_encoding = if on {
                    MouseEnc::Urxvt
                } else {
                    MouseEnc::Default
                }
            }
            2004 if self.caps.bracketed_paste => self.screen.modes.bracketed_paste = on,
            2026 if self.caps.sync_output => self.screen.modes.sync_output = on,
            47 | 1047 => self.set_alt(on, false),
            1049 => self.set_alt(on, true),
            1048 => {
                if on {
                    self.saved = Some((self.screen.cursor.row, self.screen.cursor.col, self.pen));
                } else if let Some((r, c, p)) = self.saved {
                    self.set_cur(r, c);
                    self.pen = p;
                }
            }
            _ => {}
        }
    }

    fn set_alt(&mut self, on: bool, save_clear: bool) {
        if on {
            if self.screen.active == ScreenKind::Alt {
                return;
            }
            if save_clear {
                self.saved_alt = Some((self.screen.cursor.row, self.screen.cursor.col, self.pen));
            }
            self.screen.active = ScreenKind::Alt;
            self.screen.modes.alt_screen = true;
            // clear alt grid
            let (rows, cols) = self.dims();
            for r in 0..rows {
                for c in 0..cols {
                    self.screen.alt.set(r, c, Cell::empty());
                }
            }
            if save_clear {
                self.set_cur(0, 0);
            }
        } else {
            if self.screen.active == ScreenKind::Primary {
                return;
            }
            self.screen.active = ScreenKind::Primary;
            self.screen.modes.alt_screen = false;
            if save_clear {
                if let Some((r, c, p)) = self.saved_alt.take() {
                    self.set_cur(r, c);
                    self.pen = p;
                }
            }
        }
    }

    // ---- SGR -------------------------------------------------------------

    fn apply_sgr(&mut self, params: &Params) {
        let groups: Vec<Vec<u16>> = params.iter().map(|g| g.to_vec()).collect();
        if groups.is_empty() {
            self.pen = CellStyle::default();
            return;
        }
        let mut i = 0;
        while i < groups.len() {
            let g = &groups[i];
            let code = g.first().copied().unwrap_or(0);
            match code {
                0 => self.pen = CellStyle::default(),
                1 => self.pen.attrs.insert(Attrs::BOLD),
                2 => self.pen.attrs.insert(Attrs::DIM),
                3 => self.pen.attrs.insert(Attrs::ITALIC),
                4 => {
                    // 4:3 curly via subparam
                    if g.len() > 1 && g[1] == 3 {
                        self.pen.attrs.insert(Attrs::CURLY_UNDERLINE);
                    } else if g.len() > 1 && g[1] == 2 {
                        self.pen.attrs.insert(Attrs::DOUBLE_UNDERLINE);
                    } else if g.len() > 1 && g[1] == 0 {
                        self.pen.attrs.remove(
                            Attrs::UNDERLINE | Attrs::DOUBLE_UNDERLINE | Attrs::CURLY_UNDERLINE,
                        );
                    } else {
                        self.pen.attrs.insert(Attrs::UNDERLINE);
                    }
                }
                5 => self.pen.attrs.insert(Attrs::BLINK),
                7 => self.pen.attrs.insert(Attrs::REVERSE),
                8 => self.pen.attrs.insert(Attrs::HIDDEN),
                9 => self.pen.attrs.insert(Attrs::STRIKE),
                21 => self.pen.attrs.insert(Attrs::DOUBLE_UNDERLINE),
                22 => self.pen.attrs.remove(Attrs::BOLD | Attrs::DIM),
                23 => self.pen.attrs.remove(Attrs::ITALIC),
                24 => self
                    .pen
                    .attrs
                    .remove(Attrs::UNDERLINE | Attrs::DOUBLE_UNDERLINE | Attrs::CURLY_UNDERLINE),
                25 => self.pen.attrs.remove(Attrs::BLINK),
                27 => self.pen.attrs.remove(Attrs::REVERSE),
                28 => self.pen.attrs.remove(Attrs::HIDDEN),
                29 => self.pen.attrs.remove(Attrs::STRIKE),
                30..=37 => self.pen.fg = Color::Indexed((code - 30) as u8),
                38 => {
                    if let Some((color, adv)) = parse_ext_color(&groups, i) {
                        self.pen.fg = color;
                        i += adv;
                        continue;
                    }
                }
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((code - 40) as u8),
                48 => {
                    if let Some((color, adv)) = parse_ext_color(&groups, i) {
                        self.pen.bg = color;
                        i += adv;
                        continue;
                    }
                }
                49 => self.pen.bg = Color::Default,
                58 => {
                    if let Some((color, adv)) = parse_ext_color(&groups, i) {
                        self.pen.underline = color;
                        i += adv;
                        continue;
                    }
                }
                59 => self.pen.underline = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((code - 100 + 8) as u8),
                _ => {}
            }
            i += 1;
        }
    }

    fn report_cursor(&mut self) {
        let (row, col) = self.cur();
        self.replies
            .extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
    }

    fn tab(&mut self) {
        let (_, cols) = self.dims();
        let col = self.screen.cursor.col;
        let next = ((col / self.tab_width) + 1) * self.tab_width;
        self.screen.cursor.col = next.min(cols.saturating_sub(1));
    }
}

/// Parse 38/48/58 extended color. `i` is the index of the 38/48/58 group.
/// Returns (color, groups_consumed).
fn parse_ext_color(groups: &[Vec<u16>], i: usize) -> Option<(Color, usize)> {
    let g = &groups[i];
    if g.len() >= 2 {
        // colon (subparam) form: [38, 5, n] or [38, 2, r, g, b] or [38,2,cs,r,g,b]
        match g[1] {
            5 => {
                let n = *g.get(2)? as u8;
                Some((Color::Indexed(n), 1))
            }
            2 => {
                if g.len() >= 6 {
                    Some((Color::Rgb(g[3] as u8, g[4] as u8, g[5] as u8), 1))
                } else if g.len() >= 5 {
                    Some((Color::Rgb(g[2] as u8, g[3] as u8, g[4] as u8), 1))
                } else {
                    None
                }
            }
            _ => None,
        }
    } else {
        // semicolon form: consume following groups
        let kind = groups.get(i + 1)?.first().copied()?;
        match kind {
            5 => {
                let n = *groups.get(i + 2)?.first()? as u8;
                Some((Color::Indexed(n), 3))
            }
            2 => {
                let r = *groups.get(i + 2)?.first()? as u8;
                let gg = *groups.get(i + 3)?.first()? as u8;
                let b = *groups.get(i + 4)?.first()? as u8;
                Some((Color::Rgb(r, gg, b), 5))
            }
            _ => None,
        }
    }
}

/// First subparam of param `idx`, defaulting and treating 0/missing as `dflt`.
fn pn(params: &Params, idx: usize, dflt: u16) -> u16 {
    params
        .iter()
        .nth(idx)
        .and_then(|s| s.first().copied())
        .filter(|&v| v != 0)
        .unwrap_or(dflt)
}

/// Raw first subparam (0 preserved).
fn praw(params: &Params, idx: usize, dflt: u16) -> u16 {
    params
        .iter()
        .nth(idx)
        .and_then(|s| s.first().copied())
        .unwrap_or(dflt)
}

impl vte::Perform for Term {
    fn print(&mut self, c: char) {
        let c = if self.active_charset() == Charset::DecGraphics {
            dec_graphics(c)
        } else {
            c
        };
        self.print_glyph(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => {
                self.screen.cursor.col = self.screen.cursor.col.saturating_sub(1);
                self.pending_wrap = false;
            }
            0x09 => self.tab(),
            0x0a..=0x0c => self.line_feed(),
            0x0d => self.carriage_return(),
            0x0e => self.shift_out = true,
            0x0f => self.shift_out = false,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let private = intermediates.contains(&b'?');
        let dollar = intermediates.contains(&b'$');
        match action {
            'A' => {
                let n = pn(params, 0, 1);
                self.screen.cursor.row = self.screen.cursor.row.saturating_sub(n);
            }
            'B' | 'e' => {
                let n = pn(params, 0, 1);
                let (rows, _) = self.dims();
                self.screen.cursor.row = (self.screen.cursor.row + n).min(rows.saturating_sub(1));
            }
            'C' | 'a' => {
                let n = pn(params, 0, 1);
                let (_, cols) = self.dims();
                self.screen.cursor.col = (self.screen.cursor.col + n).min(cols.saturating_sub(1));
                self.pending_wrap = false;
            }
            'D' => {
                let n = pn(params, 0, 1);
                self.screen.cursor.col = self.screen.cursor.col.saturating_sub(n);
                self.pending_wrap = false;
            }
            'E' => {
                let n = pn(params, 0, 1);
                let (rows, _) = self.dims();
                self.screen.cursor.row = (self.screen.cursor.row + n).min(rows.saturating_sub(1));
                self.screen.cursor.col = 0;
            }
            'F' => {
                let n = pn(params, 0, 1);
                self.screen.cursor.row = self.screen.cursor.row.saturating_sub(n);
                self.screen.cursor.col = 0;
            }
            'G' | '`' => {
                let c = pn(params, 0, 1) - 1;
                self.set_cur(self.screen.cursor.row, c);
                self.pending_wrap = false;
            }
            'd' => {
                let r = pn(params, 0, 1) - 1;
                self.set_cur(r, self.screen.cursor.col);
            }
            'H' | 'f' => {
                let r = pn(params, 0, 1) - 1;
                let c = pn(params, 1, 1) - 1;
                self.set_cur(r, c);
                self.pending_wrap = false;
            }
            'J' => self.erase_in_display(praw(params, 0, 0)),
            'K' => self.erase_in_line(praw(params, 0, 0)),
            'L' => self.insert_lines(pn(params, 0, 1)),
            'M' => self.delete_lines(pn(params, 0, 1)),
            '@' => self.insert_chars(pn(params, 0, 1)),
            'P' => self.delete_chars(pn(params, 0, 1)),
            'X' => self.erase_chars(pn(params, 0, 1)),
            'S' => self.scroll_up(pn(params, 0, 1)),
            'T' => self.scroll_down(pn(params, 0, 1)),
            'm' if intermediates.first() == Some(&b'>') => {
                // XTMODKEYS: CSI > 4 ; n m sets modifyOtherKeys; CSI > 4 m resets
                if self.caps.modify_other_keys && praw(params, 0, 0) == 4 {
                    self.screen.modes.modify_other_keys = praw(params, 1, 0) as u8;
                }
            }
            'm' if intermediates.first() == Some(&b'?') => {
                // XTQMODKEYS: CSI ? 4 m → CSI > 4 ; n m
                if self.caps.modify_other_keys && praw(params, 0, 0) == 4 {
                    let n = self.screen.modes.modify_other_keys;
                    self.replies
                        .extend_from_slice(format!("\x1b[>4;{n}m").as_bytes());
                }
            }
            'm' if intermediates.is_empty() => self.apply_sgr(params),
            'u' if !intermediates.is_empty() => self.kitty_keyboard(params, intermediates),
            'q' if intermediates.first() == Some(&b'>') => {
                // XTVERSION → DCS > | text ST
                if let Some(v) = self.caps.xtversion.clone() {
                    self.replies
                        .extend_from_slice(format!("\x1bP>|{v}\x1b\\").as_bytes());
                }
            }
            'r' => {
                let (rows, _) = self.dims();
                let top = pn(params, 0, 1) - 1;
                let bot = pn(params, 1, rows) - 1;
                if top < bot && bot < rows {
                    self.scroll_top = top;
                    self.scroll_bot = bot;
                    self.set_cur(0, 0);
                }
            }
            'h' if private => {
                for g in params.iter() {
                    if let Some(&m) = g.first() {
                        self.set_dec_mode(m, true);
                    }
                }
            }
            'l' if private => {
                for g in params.iter() {
                    if let Some(&m) = g.first() {
                        self.set_dec_mode(m, false);
                    }
                }
            }
            'n' if !private => {
                let p = praw(params, 0, 0);
                if p == 6 {
                    self.report_cursor();
                } else if p == 5 {
                    self.replies.extend_from_slice(b"\x1b[0n");
                }
            }
            'c' if !private => {
                if intermediates.first() == Some(&b'>') {
                    let d2 = self.da2.clone();
                    self.replies.extend_from_slice(&d2);
                } else {
                    let d1 = self.da1.clone();
                    self.replies.extend_from_slice(&d1);
                }
            }
            'q' if intermediates.first() == Some(&b' ') => {
                let shape = praw(params, 0, 1);
                self.screen.cursor.shape = match shape {
                    0..=2 => CursorShape::Block,
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
                self.screen.cursor.visible = shape != 0 || self.screen.cursor.visible;
            }
            'p' if dollar && private => {
                let m = praw(params, 0, 0);
                self.report_dec_mode(m);
            }
            'p' if dollar => {
                // ANSI-mode DECRQM: none implemented → not recognized
                let m = praw(params, 0, 0);
                self.replies
                    .extend_from_slice(format!("\x1b[{m};0$y").as_bytes());
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if let Some(&slot) = intermediates.first() {
            // SCS: ESC ( F designates G0, ESC ) F designates G1
            let set = match byte {
                b'0' => Charset::DecGraphics,
                _ => Charset::Ascii,
            };
            match slot {
                b'(' => self.g0 = set,
                b')' => self.g1 = set,
                _ => {}
            }
            return;
        }
        match byte {
            b'M' => self.reverse_index(),
            b'D' => self.line_feed(),
            b'E' => {
                self.carriage_return();
                self.line_feed();
            }
            b'7' => {
                self.saved = Some((self.screen.cursor.row, self.screen.cursor.col, self.pen));
            }
            b'8' => {
                if let Some((r, c, p)) = self.saved {
                    self.set_cur(r, c);
                    self.pen = p;
                }
            }
            b'=' => self.screen.modes.app_keypad = true,
            b'>' => self.screen.modes.app_keypad = false,
            b'c' => {
                // RIS full reset
                let (rows, cols) = self.dims();
                let (da1, da2) = (self.da1.clone(), self.da2.clone());
                let caps = self.caps.clone();
                *self = Term::new(rows, cols, da1, da2);
                self.caps = caps;
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        let cmd = params[0];
        match cmd {
            b"0" | b"2" => {
                if let Some(t) = params.get(1) {
                    self.screen.title = Some(String::from_utf8_lossy(t).to_string());
                }
            }
            b"5379" if params.get(1) == Some(&b"muse:ready".as_ref()) => {
                self.ready_pulses += 1;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vte::Parser;

    fn run(t: &mut Term, bytes: &[u8]) {
        let mut p = Parser::new();
        for &b in bytes {
            p.advance(t, b);
        }
    }

    fn term() -> Term {
        Term::new(5, 10, b"\x1b[?1c".to_vec(), b"\x1b[>0c".to_vec())
    }

    #[test]
    fn prints_text() {
        let mut t = term();
        run(&mut t, b"hi");
        assert_eq!(t.screen.active_grid().row_text_trimmed(0), "hi");
        assert_eq!(t.cur(), (0, 2));
    }

    #[test]
    fn sgr_color() {
        let mut t = term();
        run(&mut t, b"\x1b[31mA\x1b[0mB");
        assert_eq!(t.screen.primary.cell(0, 0).style.fg, Color::Indexed(1));
        assert_eq!(t.screen.primary.cell(0, 1).style.fg, Color::Default);
    }

    #[test]
    fn sgr_bold_and_reset() {
        let mut t = term();
        run(&mut t, b"\x1b[1;3mX");
        let st = t.screen.primary.cell(0, 0).style;
        assert!(st.attrs.contains(Attrs::BOLD));
        assert!(st.attrs.contains(Attrs::ITALIC));
        run(&mut t, b"\x1b[mY");
        assert_eq!(t.screen.primary.cell(0, 1).style.attrs, Attrs::empty());
    }

    #[test]
    fn sgr_256_and_rgb_semicolon() {
        let mut t = term();
        run(&mut t, b"\x1b[38;5;200mA\x1b[48;2;1;2;3mB");
        assert_eq!(t.screen.primary.cell(0, 0).style.fg, Color::Indexed(200));
        assert_eq!(t.screen.primary.cell(0, 1).style.bg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn sgr_rgb_colon() {
        let mut t = term();
        run(&mut t, b"\x1b[38:2::10:20:30mA");
        // colon form [38,2,cs,r,g,b] => len 6
        assert_eq!(t.screen.primary.cell(0, 0).style.fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn bright_colors() {
        let mut t = term();
        run(&mut t, b"\x1b[91;102mX");
        let st = t.screen.primary.cell(0, 0).style;
        assert_eq!(st.fg, Color::Indexed(9));
        assert_eq!(st.bg, Color::Indexed(10));
    }

    #[test]
    fn attr_resets() {
        let mut t = term();
        run(&mut t, b"\x1b[1;4;5;7;9mX\x1b[22;24;25;27;29mY");
        let y = t.screen.primary.cell(0, 1).style;
        assert_eq!(y.attrs, Attrs::empty());
    }

    #[test]
    fn underline_styles() {
        let mut t = term();
        run(&mut t, b"\x1b[4:3mA\x1b[4:2mB\x1b[21mC");
        assert!(t
            .screen
            .primary
            .cell(0, 0)
            .style
            .attrs
            .contains(Attrs::CURLY_UNDERLINE));
        assert!(t
            .screen
            .primary
            .cell(0, 1)
            .style
            .attrs
            .contains(Attrs::DOUBLE_UNDERLINE));
        assert!(t
            .screen
            .primary
            .cell(0, 2)
            .style
            .attrs
            .contains(Attrs::DOUBLE_UNDERLINE));
    }

    #[test]
    fn underline_color() {
        let mut t = term();
        run(&mut t, b"\x1b[58;5;9mA\x1b[59mB");
        assert_eq!(
            t.screen.primary.cell(0, 0).style.underline,
            Color::Indexed(9)
        );
        assert_eq!(t.screen.primary.cell(0, 1).style.underline, Color::Default);
    }

    #[test]
    fn cursor_movement() {
        let mut t = term();
        run(&mut t, b"\x1b[3;5H");
        assert_eq!(t.cur(), (2, 4));
        run(&mut t, b"\x1b[A");
        assert_eq!(t.cur(), (1, 4));
        run(&mut t, b"\x1b[2B");
        assert_eq!(t.cur(), (3, 4));
        run(&mut t, b"\x1b[2D");
        assert_eq!(t.cur(), (3, 2));
        run(&mut t, b"\x1b[3C");
        assert_eq!(t.cur(), (3, 5));
    }

    #[test]
    fn cha_vpa_cnl_cpl() {
        let mut t = term();
        run(&mut t, b"\x1b[5G");
        assert_eq!(t.cur().1, 4);
        run(&mut t, b"\x1b[3d");
        assert_eq!(t.cur().0, 2);
        run(&mut t, b"\x1b[E");
        assert_eq!(t.cur(), (3, 0));
        run(&mut t, b"\x1b[F");
        assert_eq!(t.cur(), (2, 0));
    }

    #[test]
    fn carriage_return_and_backspace() {
        let mut t = term();
        run(&mut t, b"abc\r");
        assert_eq!(t.cur(), (0, 0));
        run(&mut t, b"xy\x08");
        assert_eq!(t.cur(), (0, 1));
    }

    #[test]
    fn tab_stops() {
        let mut t = term();
        run(&mut t, b"\t");
        assert_eq!(t.cur().1, 8);
        run(&mut t, b"\t");
        assert_eq!(t.cur().1, 9); // clamped to cols-1
    }

    #[test]
    fn linefeed_and_scroll() {
        let mut t = term();
        run(&mut t, b"l0\r\nl1\r\nl2\r\nl3\r\nl4");
        assert_eq!(t.screen.primary.row_text_trimmed(4), "l4");
        // scroll: another newline pushes l0 into scrollback
        run(&mut t, b"\r\nl5");
        assert_eq!(t.screen.primary.row_text_trimmed(4), "l5");
        assert_eq!(t.screen.primary.row_text_trimmed(0), "l1");
        assert_eq!(t.screen.scrollback.len(), 1);
        assert_eq!(
            String::from_utf8(
                t.screen.scrollback[0]
                    .iter()
                    .map(|c| c.text().as_bytes().first().copied().unwrap_or(b' '))
                    .collect()
            )
            .unwrap()
            .trim_end(),
            "l0"
        );
    }

    #[test]
    fn autowrap() {
        let mut t = term();
        run(&mut t, b"0123456789X");
        // 10 cols filled, X wraps to row 1 col 0
        assert_eq!(t.screen.primary.row_text_trimmed(0), "0123456789");
        assert_eq!(t.screen.primary.cell(1, 0).text(), "X");
    }

    #[test]
    fn autowrap_disabled() {
        let mut t = term();
        run(&mut t, b"\x1b[?7l");
        run(&mut t, b"0123456789XY");
        assert_eq!(t.cur().0, 0);
        // last col overwritten
        assert_eq!(t.screen.primary.cell(0, 9).text(), "Y");
    }

    #[test]
    fn wide_char_and_spacer() {
        let mut t = term();
        run(&mut t, "日x".as_bytes());
        assert_eq!(t.screen.primary.cell(0, 0).text(), "日");
        assert!(matches!(t.screen.primary.cell(0, 1).kind, CellKind::Spacer));
        assert_eq!(t.screen.primary.cell(0, 2).text(), "x");
    }

    #[test]
    fn wide_char_wraps_when_no_room() {
        let mut t = term();
        // move to last col then print wide char
        run(&mut t, b"012345678");
        assert_eq!(t.cur().1, 9);
        run(&mut t, "日".as_bytes());
        // wrapped to next row
        assert_eq!(t.screen.primary.cell(1, 0).text(), "日");
    }

    #[test]
    fn combining_mark_attaches() {
        let mut t = term();
        run(&mut t, "e\u{0301}".as_bytes());
        assert_eq!(t.screen.primary.cell(0, 0).text(), "e\u{0301}");
        assert_eq!(t.cur().1, 1);
    }

    #[test]
    fn erase_in_line() {
        let mut t = term();
        run(&mut t, b"hello\r\x1b[2C\x1b[0K");
        assert_eq!(t.screen.primary.row_text_trimmed(0), "he");
    }

    #[test]
    fn erase_line_left() {
        let mut t = term();
        run(&mut t, b"hello\x1b[3G\x1b[1K");
        // erase cols 0..=2 (cursor at col 2)
        assert_eq!(t.screen.primary.cell(0, 0).text(), " ");
        assert_eq!(t.screen.primary.cell(0, 3).text(), "l");
    }

    #[test]
    fn erase_display() {
        let mut t = term();
        run(&mut t, b"a\r\nb\r\nc\x1b[2J");
        for r in 0..5 {
            assert_eq!(t.screen.primary.row_text_trimmed(r), "");
        }
    }

    #[test]
    fn erase_display_below_and_above() {
        let mut t = term();
        run(&mut t, b"AAAA\r\nBBBB\r\nCCCC\x1b[2;2H\x1b[0J");
        assert_eq!(t.screen.primary.row_text_trimmed(0), "AAAA");
        assert_eq!(t.screen.primary.row_text_trimmed(2), "");
        run(&mut t, b"\x1b[1J");
        assert_eq!(t.screen.primary.row_text_trimmed(0), "");
    }

    #[test]
    fn insert_delete_chars() {
        let mut t = term();
        run(&mut t, b"abcde\r\x1b[2C\x1b[2@");
        // insert 2 blanks at col 2: "ab  cde"
        assert_eq!(t.screen.primary.row_text_trimmed(0), "ab  cde");
        run(&mut t, b"\r\x1b[2P");
        // delete 2 chars at col 0: "  cde"
        assert_eq!(t.screen.primary.row_text_trimmed(0), "  cde");
    }

    #[test]
    fn erase_chars_ech() {
        let mut t = term();
        run(&mut t, b"abcde\r\x1b[3X");
        assert_eq!(t.screen.primary.row_text_trimmed(0), "   de");
    }

    #[test]
    fn insert_delete_lines() {
        let mut t = term();
        run(&mut t, b"l0\r\nl1\r\nl2\x1b[1;1H\x1b[1L");
        assert_eq!(t.screen.primary.row_text_trimmed(0), "");
        assert_eq!(t.screen.primary.row_text_trimmed(1), "l0");
        run(&mut t, b"\x1b[1M");
        assert_eq!(t.screen.primary.row_text_trimmed(0), "l0");
    }

    #[test]
    fn scroll_su_sd() {
        let mut t = term();
        run(&mut t, b"l0\r\nl1\r\nl2\x1b[S");
        assert_eq!(t.screen.primary.row_text_trimmed(0), "l1");
        run(&mut t, b"\x1b[T");
        assert_eq!(t.screen.primary.row_text_trimmed(1), "l1");
    }

    #[test]
    fn scroll_region() {
        let mut t = term();
        run(&mut t, b"l0\r\nl1\r\nl2\r\nl3\r\nl4");
        // set region rows 2..4 (1-indexed 2;4 => idx1..3)
        run(&mut t, b"\x1b[2;4r");
        // cursor homed to 0,0
        assert_eq!(t.cur(), (0, 0));
    }

    #[test]
    fn alt_screen_switch() {
        let mut t = term();
        run(&mut t, b"primary\x1b[?1049h");
        assert_eq!(t.screen.active, ScreenKind::Alt);
        assert!(t.screen.modes.alt_screen);
        run(&mut t, b"alt");
        assert_eq!(t.screen.active_grid().row_text_trimmed(0), "alt");
        run(&mut t, b"\x1b[?1049l");
        assert_eq!(t.screen.active, ScreenKind::Primary);
        assert_eq!(t.screen.active_grid().row_text_trimmed(0), "primary");
    }

    #[test]
    fn dec_modes() {
        let mut t = term();
        run(
            &mut t,
            b"\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?2026h\x1b[?25l",
        );
        assert!(t.screen.modes.app_cursor_keys);
        assert!(t.screen.modes.bracketed_paste);
        assert_eq!(t.screen.modes.mouse, MouseMode::Normal);
        assert_eq!(t.screen.modes.mouse_encoding, MouseEnc::Sgr);
        assert!(t.screen.modes.sync_output);
        assert!(!t.screen.cursor.visible);
        run(&mut t, b"\x1b[?1l\x1b[?1000l\x1b[?1006l");
        assert!(!t.screen.modes.app_cursor_keys);
        assert_eq!(t.screen.modes.mouse, MouseMode::Off);
        assert_eq!(t.screen.modes.mouse_encoding, MouseEnc::Default);
    }

    #[test]
    fn mouse_button_any_event_modes() {
        let mut t = term();
        run(&mut t, b"\x1b[?1002h");
        assert_eq!(t.screen.modes.mouse, MouseMode::ButtonEvent);
        run(&mut t, b"\x1b[?1003h");
        assert_eq!(t.screen.modes.mouse, MouseMode::AnyEvent);
    }

    #[test]
    fn da_replies() {
        let mut t = term();
        run(&mut t, b"\x1b[c");
        assert_eq!(t.replies, b"\x1b[?1c");
        t.replies.clear();
        run(&mut t, b"\x1b[>c");
        assert_eq!(t.replies, b"\x1b[>0c");
    }

    #[test]
    fn dsr_cursor_report() {
        let mut t = term();
        run(&mut t, b"\x1b[2;3H\x1b[6n");
        assert_eq!(t.replies, b"\x1b[2;3R");
        t.replies.clear();
        run(&mut t, b"\x1b[5n");
        assert_eq!(t.replies, b"\x1b[0n");
    }

    #[test]
    fn save_restore_cursor_decsc() {
        let mut t = term();
        run(&mut t, b"\x1b[3;3H\x1b7\x1b[1;1H\x1b8");
        assert_eq!(t.cur(), (2, 2));
    }

    #[test]
    fn reverse_index() {
        let mut t = term();
        run(&mut t, b"\x1b[1;1H\x1bM");
        // at top, reverse index scrolls down
        assert_eq!(t.cur(), (0, 0));
        run(&mut t, b"\x1b[3;1H\x1bM");
        assert_eq!(t.cur().0, 1);
    }

    #[test]
    fn nel_and_ind() {
        let mut t = term();
        run(&mut t, b"ab\x1bE");
        assert_eq!(t.cur(), (1, 0));
        run(&mut t, b"\x1bD");
        assert_eq!(t.cur().0, 2);
    }

    #[test]
    fn ris_reset() {
        let mut t = term();
        run(&mut t, b"\x1b[31mhello\x1bc");
        assert_eq!(t.screen.primary.row_text_trimmed(0), "");
        assert_eq!(t.cur(), (0, 0));
    }

    #[test]
    fn keypad_modes() {
        let mut t = term();
        run(&mut t, b"\x1b=");
        assert!(t.screen.modes.app_keypad);
        run(&mut t, b"\x1b>");
        assert!(!t.screen.modes.app_keypad);
    }

    #[test]
    fn cursor_shape() {
        let mut t = term();
        run(&mut t, b"\x1b[4 q");
        assert_eq!(t.screen.cursor.shape, CursorShape::Underline);
        run(&mut t, b"\x1b[6 q");
        assert_eq!(t.screen.cursor.shape, CursorShape::Bar);
    }

    #[test]
    fn osc_title() {
        let mut t = term();
        run(&mut t, b"\x1b]0;My Title\x07");
        assert_eq!(t.screen.title.as_deref(), Some("My Title"));
        run(&mut t, b"\x1b]2;Other\x1b\\");
        assert_eq!(t.screen.title.as_deref(), Some("Other"));
    }

    #[test]
    fn osc_ready_marker() {
        let mut t = term();
        run(&mut t, b"\x1b]5379;muse:ready\x07");
        assert_eq!(t.ready_pulses, 1);
    }

    #[test]
    fn resize_preserves_topleft() {
        let mut t = term();
        run(&mut t, b"hello");
        t.resize(20, 10);
        assert_eq!(t.screen.primary.dims(), (10, 20));
        assert_eq!(t.screen.primary.row_text_trimmed(0), "hello");
    }

    #[test]
    fn resize_shrink_clamps_cursor() {
        let mut t = term();
        run(&mut t, b"\x1b[5;9H");
        t.resize(4, 3);
        assert!(t.cur().0 < 3 && t.cur().1 < 4);
    }

    #[test]
    fn decrqm_reply() {
        let mut t = term();
        run(&mut t, b"\x1b[?2026$p");
        assert!(t.replies.starts_with(b"\x1b[?2026;"));
    }

    #[test]
    fn cursor_shape_hidden_then_block() {
        let mut t = term();
        run(&mut t, b"\x1b[0 q");
        assert_eq!(t.screen.cursor.shape, CursorShape::Block);
    }

    #[test]
    fn urxvt_and_utf8_mouse_encodings() {
        let mut t = term();
        run(&mut t, b"\x1b[?1015h");
        assert_eq!(t.screen.modes.mouse_encoding, MouseEnc::Urxvt);
        run(&mut t, b"\x1b[?1005h");
        assert_eq!(t.screen.modes.mouse_encoding, MouseEnc::Utf8);
        run(&mut t, b"\x1b[?1005l");
        assert_eq!(t.screen.modes.mouse_encoding, MouseEnc::Default);
    }

    #[test]
    fn save_restore_cursor_1048() {
        let mut t = term();
        run(&mut t, b"\x1b[3;3H\x1b[?1048h\x1b[1;1H\x1b[?1048l");
        assert_eq!(t.cur(), (2, 2));
    }

    #[test]
    fn alt_screen_47_no_clear_save() {
        let mut t = term();
        run(&mut t, b"primary\x1b[?47h");
        assert_eq!(t.screen.active, ScreenKind::Alt);
        run(&mut t, b"\x1b[?47l");
        assert_eq!(t.screen.active, ScreenKind::Primary);
    }

    #[test]
    fn double_alt_enter_is_noop() {
        let mut t = term();
        run(&mut t, b"\x1b[?1049h\x1b[?1049h");
        assert_eq!(t.screen.active, ScreenKind::Alt);
        run(&mut t, b"\x1b[?1049l\x1b[?1049l");
        assert_eq!(t.screen.active, ScreenKind::Primary);
    }

    #[test]
    fn unknown_dec_mode_ignored() {
        let mut t = term();
        run(&mut t, b"\x1b[?9999h");
        // no panic, no state change of interest
        assert!(!t.screen.modes.alt_screen);
    }

    #[test]
    fn insert_delete_lines_outside_region_noop() {
        let mut t = term();
        run(&mut t, b"\x1b[2;3r"); // region rows 1..2
        run(&mut t, b"\x1b[1;1H\x1b[1L"); // cursor at row 0, outside region
                                          // no panic; row 0 unaffected by IL
        assert_eq!(t.cur().0, 0);
    }

    #[test]
    fn charset_designation_ignored() {
        let mut t = term();
        run(&mut t, b"\x1b(B");
        run(&mut t, b"ok");
        assert_eq!(t.screen.primary.row_text_trimmed(0), "ok");
    }

    #[test]
    fn osc_unknown_ignored() {
        let mut t = term();
        run(&mut t, b"\x1b]9;notification\x07");
        assert!(t.screen.title.is_none());
    }

    #[test]
    fn combining_on_empty_dropped() {
        let mut t = term();
        // combining mark with no preceding glyph
        run(&mut t, "\u{0301}".as_bytes());
        assert_eq!(t.cur(), (0, 0));
    }

    #[test]
    fn combining_skips_spacer_to_glyph() {
        let mut t = term();
        run(&mut t, "日".as_bytes()); // glyph + spacer, cursor at col 2
        run(&mut t, "\u{0301}".as_bytes()); // should attach to 日
        assert!(t.screen.primary.cell(0, 0).text().contains('\u{0301}'));
    }

    #[test]
    fn ind_at_bottom_scrolls() {
        let mut t = term();
        run(&mut t, b"\x1b[5;1H"); // last row
        run(&mut t, b"\x1bD"); // IND
        assert_eq!(t.cur().0, 4); // stayed at bottom (scrolled)
    }

    #[test]
    fn private_dsr_ignored() {
        let mut t = term();
        run(&mut t, b"\x1b[?6n");
        assert!(t.replies.is_empty());
    }

    #[test]
    fn ech_does_not_move_cursor() {
        let mut t = term();
        run(&mut t, b"abcd\x1b[1;1H\x1b[2X");
        assert_eq!(t.cur(), (0, 0));
        assert_eq!(t.screen.primary.row_text_trimmed(0), "  cd");
    }
}
