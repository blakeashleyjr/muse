//! The locator engine: query a screen for regions/text/styles (§10).
//!
//! Resolution is stateless and pure. The actor wraps it in deadline polling
//! for web-first retry.

use crate::color::Color;
use crate::grid::{Grid, Rect};
use crate::screen::Screen;
use crate::style::{Attrs, CellStyle};
use serde::{Deserialize, Serialize};

/// A style predicate for the `Styled` locator.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StylePredicate {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    /// Cell must contain *all* of these attrs.
    pub attrs_all: Attrs,
    /// Cell must contain *at least one* of these attrs (empty = no constraint).
    pub attrs_any: Attrs,
}

impl StylePredicate {
    pub fn matches(&self, st: &CellStyle) -> bool {
        if let Some(fg) = self.fg {
            if st.fg != fg {
                return false;
            }
        }
        if let Some(bg) = self.bg {
            if st.bg != bg {
                return false;
            }
        }
        if !self.attrs_all.is_empty() && !st.attrs.contains(self.attrs_all) {
            return false;
        }
        if !self.attrs_any.is_empty() && !st.attrs.intersects(self.attrs_any) {
            return false;
        }
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locator {
    Text {
        pattern: String,
        ignore_case: bool,
        whole_line: bool,
    },
    Regex {
        re: String,
    },
    Cell {
        row: u16,
        col: u16,
    },
    Region {
        rect: Rect,
    },
    Styled {
        text: Option<String>,
        pred: StylePredicate,
    },
    Cursor,
    Line {
        row: u16,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Match {
    pub rect: Rect,
    pub text: String,
    pub styles: Vec<(Rect, CellStyle)>,
}

/// A logical buffer over a region of the grid, with a byte→(row,col) map so a
/// text/regex hit can be translated back to grid coordinates respecting wide
/// glyphs.
struct Buf {
    text: String,
    /// One entry per *byte* of `text`: the (row, col) that byte belongs to.
    pos: Vec<(u16, u16)>,
}

fn build_buf(grid: &Grid, r0: u16, r1: u16, join_newline: bool) -> Buf {
    let mut text = String::new();
    let mut pos: Vec<(u16, u16)> = Vec::new();
    for r in r0..r1 {
        let (_, cols) = grid.dims();
        for c in 0..cols {
            let cell = grid.cell(r, c);
            let t = cell.text();
            for _ in 0..t.len() {
                pos.push((r, c));
            }
            text.push_str(t);
        }
        if join_newline && r + 1 < r1 {
            pos.push((r, grid.cols()));
            text.push('\n');
        }
    }
    Buf { text, pos }
}

/// Given a byte range in a buffer, compute the bounding rect + styles.
fn range_to_match(screen: &Screen, buf: &Buf, start: usize, end: usize) -> Match {
    let grid = screen.active_grid();
    debug_assert!(end > start);
    let mut min_r = u16::MAX;
    let mut max_r = 0u16;
    let mut min_c = u16::MAX;
    let mut max_c = 0u16;
    let mut last_col = 0u16;
    let mut last_row = 0u16;
    for &(r, c) in &buf.pos[start..end] {
        // skip the newline sentinel column (== cols)
        if c >= grid.cols() {
            continue;
        }
        min_r = min_r.min(r);
        max_r = max_r.max(r);
        min_c = min_c.min(c);
        max_c = max_c.max(c);
        last_col = c;
        last_row = r;
    }
    let last_w = grid.cell(last_row, last_col).width().max(1) as u16;
    let rect = Rect {
        row: min_r,
        col: min_c,
        w: (max_c + last_w).saturating_sub(min_c),
        h: max_r - min_r + 1,
    };
    let mut styles = Vec::new();
    for (r, c) in rect.coords() {
        styles.push((Rect::cell(r, c), grid.cell(r, c).style));
    }
    Match {
        rect,
        text: buf.text[start..end].to_string(),
        styles,
    }
}

/// Resolve a locator against a screen. Pure & stateless.
pub fn resolve(screen: &Screen, loc: &Locator, multiline: bool) -> Vec<Match> {
    let grid = screen.active_grid();
    let (rows, cols) = grid.dims();
    match loc {
        Locator::Cursor => {
            let cur = screen.cursor;
            let r = cur.row.min(rows.saturating_sub(1));
            let c = cur.col.min(cols.saturating_sub(1));
            vec![Match {
                rect: Rect::cell(r, c),
                text: grid.cell(r, c).text().to_string(),
                styles: vec![(Rect::cell(r, c), grid.cell(r, c).style)],
            }]
        }
        Locator::Cell { row, col } => {
            if *row >= rows || *col >= cols {
                return vec![];
            }
            vec![Match {
                rect: Rect::cell(*row, *col),
                text: grid.cell(*row, *col).text().to_string(),
                styles: vec![(Rect::cell(*row, *col), grid.cell(*row, *col).style)],
            }]
        }
        Locator::Line { row } => {
            if *row >= rows {
                return vec![];
            }
            let buf = build_buf(grid, *row, *row + 1, false);
            let trimmed = buf.text.trim_end();
            if trimmed.is_empty() {
                return vec![Match {
                    rect: Rect::new(*row, 0, 0, 1),
                    text: String::new(),
                    styles: vec![],
                }];
            }
            vec![range_to_match(screen, &buf, 0, trimmed.len())]
        }
        Locator::Region { rect } => {
            // Clamp to grid.
            if rect.row >= rows || rect.col >= cols || rect.w == 0 || rect.h == 0 {
                return vec![];
            }
            let mut text = String::new();
            let mut styles = Vec::new();
            let r1 = (rect.row + rect.h).min(rows);
            let c1 = (rect.col + rect.w).min(cols);
            for r in rect.row..r1 {
                for c in rect.col..c1 {
                    text.push_str(grid.cell(r, c).text());
                    styles.push((Rect::cell(r, c), grid.cell(r, c).style));
                }
            }
            vec![Match {
                rect: *rect,
                text,
                styles,
            }]
        }
        Locator::Text {
            pattern,
            ignore_case,
            whole_line,
        } => resolve_text(screen, pattern, *ignore_case, *whole_line, multiline),
        Locator::Regex { re } => resolve_regex(screen, re, multiline),
        Locator::Styled { text, pred } => resolve_styled(screen, text.as_deref(), pred),
    }
}

fn resolve_text(
    screen: &Screen,
    pattern: &str,
    ignore_case: bool,
    whole_line: bool,
    multiline: bool,
) -> Vec<Match> {
    let grid = screen.active_grid();
    let (rows, _) = grid.dims();
    let mut out = Vec::new();
    if pattern.is_empty() {
        return out;
    }
    let needle = if ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    let search_rows: Vec<(u16, u16)> = if multiline {
        vec![(0, rows)]
    } else {
        (0..rows).map(|r| (r, r + 1)).collect()
    };
    for (r0, r1) in search_rows {
        let buf = build_buf(grid, r0, r1, multiline);
        let hay = if ignore_case {
            buf.text.to_lowercase()
        } else {
            buf.text.clone()
        };
        if whole_line {
            // Match only if the trimmed line equals the pattern.
            if hay.trim_end() == needle {
                let trimmed = buf.text.trim_end();
                out.push(range_to_match(
                    screen,
                    &buf,
                    0,
                    trimmed.len().max(1).min(buf.text.len()),
                ));
            }
            continue;
        }
        let mut start = 0usize;
        while let Some(rel) = hay[start..].find(&needle) {
            let s = start + rel;
            let e = s + needle.len();
            out.push(range_to_match(screen, &buf, s, e));
            start = e.max(s + 1);
            if start >= hay.len() {
                break;
            }
        }
    }
    out
}

fn resolve_regex(screen: &Screen, re_src: &str, multiline: bool) -> Vec<Match> {
    let grid = screen.active_grid();
    let (rows, _) = grid.dims();
    let re = match regex::RegexBuilder::new(re_src).multi_line(true).build() {
        Ok(re) => re,
        Err(_) => return vec![],
    };
    let mut out = Vec::new();
    let search_rows: Vec<(u16, u16)> = if multiline {
        vec![(0, rows)]
    } else {
        (0..rows).map(|r| (r, r + 1)).collect()
    };
    for (r0, r1) in search_rows {
        let buf = build_buf(grid, r0, r1, multiline);
        for m in re.find_iter(&buf.text) {
            if m.start() == m.end() {
                continue;
            }
            out.push(range_to_match(screen, &buf, m.start(), m.end()));
        }
    }
    out
}

fn resolve_styled(screen: &Screen, text: Option<&str>, pred: &StylePredicate) -> Vec<Match> {
    let grid = screen.active_grid();
    let (rows, cols) = grid.dims();
    let mut out = Vec::new();
    for r in 0..rows {
        let mut c = 0u16;
        while c < cols {
            if grid.cell(r, c).is_blank()
                && !matches!(grid.cell(r, c).kind, crate::cell::CellKind::Empty)
            {
                c += 1;
                continue;
            }
            if pred.matches(&grid.cell(r, c).style) && !grid.cell(r, c).is_blank() {
                // start a run
                let start = c;
                let mut run_text = String::new();
                let mut styles = Vec::new();
                while c < cols
                    && !grid.cell(r, c).is_blank()
                    && pred.matches(&grid.cell(r, c).style)
                {
                    run_text.push_str(grid.cell(r, c).text());
                    let w = grid.cell(r, c).width().max(1) as u16;
                    for cc in c..(c + w).min(cols) {
                        styles.push((Rect::cell(r, cc), grid.cell(r, cc).style));
                    }
                    c += w;
                }
                let rect = Rect::new(r, start, c - start, 1);
                if let Some(t) = text {
                    if !run_text.contains(t) {
                        continue;
                    }
                }
                out.push(Match {
                    rect,
                    text: run_text,
                    styles,
                });
            } else {
                c += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::{Cell, CellKind};

    fn put(s: &mut Screen, r: u16, c: u16, ch: &str, style: CellStyle) {
        s.primary.set(r, c, Cell::glyph(ch, style));
    }

    fn text_screen(line: &str) -> Screen {
        let mut s = Screen::new(3, 20);
        for (i, ch) in line.chars().enumerate() {
            put(&mut s, 0, i as u16, &ch.to_string(), CellStyle::default());
        }
        s
    }

    #[test]
    fn text_simple() {
        let s = text_screen("hello world");
        let m = resolve(
            &s,
            &Locator::Text {
                pattern: "world".into(),
                ignore_case: false,
                whole_line: false,
            },
            false,
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text, "world");
        assert_eq!(m[0].rect.col, 6);
        assert_eq!(m[0].rect.w, 5);
    }

    #[test]
    fn text_wide_glyph_column_mapping() {
        // 日本x on row 0: 日 col0(+spacer1), 本 col2(+spacer3), x col4
        let mut s = Screen::new(1, 6);
        put(&mut s, 0, 0, "日", CellStyle::default());
        s.primary.set(
            0,
            1,
            Cell {
                kind: CellKind::Spacer,
                style: CellStyle::default(),
            },
        );
        put(&mut s, 0, 2, "本", CellStyle::default());
        s.primary.set(
            0,
            3,
            Cell {
                kind: CellKind::Spacer,
                style: CellStyle::default(),
            },
        );
        put(&mut s, 0, 4, "x", CellStyle::default());
        let m = resolve(
            &s,
            &Locator::Text {
                pattern: "x".into(),
                ignore_case: false,
                whole_line: false,
            },
            false,
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].rect.col, 4);
    }

    #[test]
    fn text_ignore_case() {
        let s = text_screen("Hello");
        let m = resolve(
            &s,
            &Locator::Text {
                pattern: "hello".into(),
                ignore_case: true,
                whole_line: false,
            },
            false,
        );
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn text_whole_line() {
        let s = text_screen("prompt");
        let m = resolve(
            &s,
            &Locator::Text {
                pattern: "prompt".into(),
                ignore_case: false,
                whole_line: true,
            },
            false,
        );
        assert_eq!(m.len(), 1);
        let none = resolve(
            &s,
            &Locator::Text {
                pattern: "promp".into(),
                ignore_case: false,
                whole_line: true,
            },
            false,
        );
        assert!(none.is_empty());
    }

    #[test]
    fn text_empty_pattern_no_match() {
        let s = text_screen("abc");
        assert!(resolve(
            &s,
            &Locator::Text {
                pattern: "".into(),
                ignore_case: false,
                whole_line: false
            },
            false
        )
        .is_empty());
    }

    #[test]
    fn text_multiple_hits_in_line() {
        let s = text_screen("a a a");
        let m = resolve(
            &s,
            &Locator::Text {
                pattern: "a".into(),
                ignore_case: false,
                whole_line: false,
            },
            false,
        );
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn regex_anchored_prompt() {
        let mut s = Screen::new(2, 10);
        put(&mut s, 0, 0, "$", CellStyle::default());
        put(&mut s, 0, 1, " ", CellStyle::default());
        put(&mut s, 1, 0, "x", CellStyle::default());
        put(&mut s, 1, 1, "$", CellStyle::default());
        let m = resolve(&s, &Locator::Regex { re: "^\\$ ".into() }, false);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].rect.row, 0);
    }

    #[test]
    fn regex_invalid_returns_empty() {
        let s = text_screen("abc");
        assert!(resolve(&s, &Locator::Regex { re: "(".into() }, false).is_empty());
    }

    #[test]
    fn regex_multiline_spans_rows() {
        let mut s = Screen::new(2, 5);
        put(&mut s, 0, 0, "a", CellStyle::default());
        put(&mut s, 1, 0, "b", CellStyle::default());
        let m = resolve(
            &s,
            &Locator::Regex {
                re: "a.*\\n.*b".into(),
            },
            true,
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].rect.h, 2);
    }

    #[test]
    fn cell_locator() {
        let s = text_screen("hi");
        let m = resolve(&s, &Locator::Cell { row: 0, col: 1 }, false);
        assert_eq!(m[0].text, "i");
        assert!(resolve(&s, &Locator::Cell { row: 99, col: 0 }, false).is_empty());
    }

    #[test]
    fn line_locator() {
        let s = text_screen("hello");
        let m = resolve(&s, &Locator::Line { row: 0 }, false);
        assert_eq!(m[0].text, "hello");
        // empty line
        let m2 = resolve(&s, &Locator::Line { row: 1 }, false);
        assert_eq!(m2[0].text, "");
        assert!(resolve(&s, &Locator::Line { row: 99 }, false).is_empty());
    }

    #[test]
    fn region_locator() {
        let s = text_screen("hello");
        let m = resolve(
            &s,
            &Locator::Region {
                rect: Rect::new(0, 0, 3, 1),
            },
            false,
        );
        assert_eq!(m[0].text, "hel");
        assert!(resolve(
            &s,
            &Locator::Region {
                rect: Rect::new(0, 0, 0, 0)
            },
            false
        )
        .is_empty());
        assert!(resolve(
            &s,
            &Locator::Region {
                rect: Rect::new(99, 0, 1, 1)
            },
            false
        )
        .is_empty());
    }

    #[test]
    fn region_clamps_oversize() {
        let s = text_screen("hi");
        let m = resolve(
            &s,
            &Locator::Region {
                rect: Rect::new(0, 0, 100, 100),
            },
            false,
        );
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn cursor_locator() {
        let mut s = text_screen("hi");
        s.cursor.row = 0;
        s.cursor.col = 1;
        let m = resolve(&s, &Locator::Cursor, false);
        assert_eq!(m[0].rect, Rect::cell(0, 1));
    }

    #[test]
    fn cursor_clamps() {
        let mut s = Screen::new(2, 2);
        s.cursor.row = 50;
        s.cursor.col = 50;
        let m = resolve(&s, &Locator::Cursor, false);
        assert_eq!(m[0].rect, Rect::cell(1, 1));
    }

    #[test]
    fn styled_by_fg() {
        let mut s = Screen::new(1, 10);
        let red = CellStyle {
            fg: Color::Indexed(1),
            ..Default::default()
        };
        put(&mut s, 0, 0, "R", red);
        put(&mut s, 0, 1, "E", red);
        put(&mut s, 0, 2, "D", CellStyle::default());
        let pred = StylePredicate {
            fg: Some(Color::Indexed(1)),
            ..Default::default()
        };
        let m = resolve(&s, &Locator::Styled { text: None, pred }, false);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text, "RE");
    }

    #[test]
    fn styled_with_text_filter() {
        let mut s = Screen::new(1, 10);
        let red = CellStyle {
            fg: Color::Indexed(1),
            ..Default::default()
        };
        put(&mut s, 0, 0, "O", red);
        put(&mut s, 0, 1, "K", red);
        let pred = StylePredicate {
            fg: Some(Color::Indexed(1)),
            ..Default::default()
        };
        let m = resolve(
            &s,
            &Locator::Styled {
                text: Some("OK".into()),
                pred: pred.clone(),
            },
            false,
        );
        assert_eq!(m.len(), 1);
        let none = resolve(
            &s,
            &Locator::Styled {
                text: Some("NO".into()),
                pred,
            },
            false,
        );
        assert!(none.is_empty());
    }

    #[test]
    fn style_predicate_all_constraints() {
        let st = CellStyle {
            fg: Color::Indexed(1),
            bg: Color::Indexed(2),
            attrs: Attrs::BOLD | Attrs::ITALIC,
            underline: Color::Default,
        };
        assert!(StylePredicate {
            fg: Some(Color::Indexed(1)),
            bg: Some(Color::Indexed(2)),
            attrs_all: Attrs::BOLD,
            attrs_any: Attrs::ITALIC
        }
        .matches(&st));
        assert!(!StylePredicate {
            fg: Some(Color::Indexed(9)),
            ..Default::default()
        }
        .matches(&st));
        assert!(!StylePredicate {
            bg: Some(Color::Indexed(9)),
            ..Default::default()
        }
        .matches(&st));
        assert!(!StylePredicate {
            attrs_all: Attrs::STRIKE,
            ..Default::default()
        }
        .matches(&st));
        assert!(!StylePredicate {
            attrs_any: Attrs::STRIKE,
            ..Default::default()
        }
        .matches(&st));
    }
}
