//! Normalize + mask rules applied to text/styled before diffing (§12).

use muse_core::grid::Rect;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// The sentinel a masked cell becomes.
pub const SENTINEL: char = '\u{2588}';

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaskRule {
    Rect(Rect),
    /// A regex; matches are replaced with the sentinel.
    Content(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizeRule {
    pub re: String,
    pub replace: String,
}

/// Apply normalize rules (regex replace) to a text blob.
pub fn apply_normalize(text: &str, rules: &[NormalizeRule]) -> String {
    let mut out = text.to_string();
    for rule in rules {
        if let Ok(re) = Regex::new(&rule.re) {
            out = re.replace_all(&out, rule.replace.as_str()).into_owned();
        }
    }
    out
}

/// Apply content masks: replace each regex match with sentinels (one per char).
pub fn apply_content_masks(text: &str, masks: &[MaskRule]) -> String {
    let mut out = text.to_string();
    for m in masks {
        if let MaskRule::Content(re_src) = m {
            if let Ok(re) = Regex::new(re_src) {
                out = re
                    .replace_all(&out, |caps: &regex::Captures| {
                        SENTINEL.to_string().repeat(caps[0].chars().count())
                    })
                    .into_owned();
            }
        }
    }
    out
}

/// Apply rect masks to text lines: replace columns within each rect with the
/// sentinel (char-column approximation).
pub fn apply_rect_masks_text(text: &str, masks: &[MaskRule]) -> String {
    let rects: Vec<Rect> = masks
        .iter()
        .filter_map(|m| match m {
            MaskRule::Rect(r) => Some(*r),
            _ => None,
        })
        .collect();
    if rects.is_empty() {
        return text.to_string();
    }
    let mut lines: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
    for rect in &rects {
        for r in rect.row..rect.row.saturating_add(rect.h) {
            if let Some(line) = lines.get_mut(r as usize) {
                for c in rect.col..rect.col.saturating_add(rect.w) {
                    let ci = c as usize;
                    if ci < line.len() {
                        line[ci] = SENTINEL;
                    } else {
                        while line.len() < ci {
                            line.push(' ');
                        }
                        line.push(SENTINEL);
                    }
                }
            }
        }
    }
    lines
        .into_iter()
        .map(|l| l.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Full text transform: normalize, then content masks, then rect masks.
pub fn transform_text(text: &str, normalize: &[NormalizeRule], masks: &[MaskRule]) -> String {
    let t = apply_normalize(text, normalize);
    let t = apply_content_masks(&t, masks);
    apply_rect_masks_text(&t, masks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_replaces() {
        let rules = vec![NormalizeRule {
            re: r"\d{4}-\d{2}-\d{2}".into(),
            replace: "<DATE>".into(),
        }];
        assert_eq!(
            apply_normalize("today 2024-01-02 ok", &rules),
            "today <DATE> ok"
        );
    }

    #[test]
    fn normalize_bad_regex_skipped() {
        let rules = vec![NormalizeRule {
            re: "(".into(),
            replace: "x".into(),
        }];
        assert_eq!(apply_normalize("abc", &rules), "abc");
    }

    #[test]
    fn content_mask_sentinel_length() {
        let masks = vec![MaskRule::Content(r"\d+".into())];
        let out = apply_content_masks("id=12345", &masks);
        assert_eq!(out, format!("id={}", SENTINEL.to_string().repeat(5)));
    }

    #[test]
    fn rect_mask_replaces_columns() {
        let masks = vec![MaskRule::Rect(Rect::new(0, 1, 2, 1))];
        let out = apply_rect_masks_text("abcd", &masks);
        let chars: Vec<char> = out.chars().collect();
        assert_eq!(chars[0], 'a');
        assert_eq!(chars[1], SENTINEL);
        assert_eq!(chars[2], SENTINEL);
        assert_eq!(chars[3], 'd');
    }

    #[test]
    fn rect_mask_extends_short_line() {
        let masks = vec![MaskRule::Rect(Rect::new(0, 5, 1, 1))];
        let out = apply_rect_masks_text("ab", &masks);
        assert!(out.contains(SENTINEL));
    }

    #[test]
    fn rect_mask_no_rects_passthrough() {
        let masks = vec![MaskRule::Content("x".into())];
        assert_eq!(apply_rect_masks_text("abc", &masks), "abc");
    }

    #[test]
    fn transform_pipeline() {
        let norm = vec![NormalizeRule {
            re: r"\d+".into(),
            replace: "N".into(),
        }];
        let masks = vec![MaskRule::Content("N".into())];
        let out = transform_text("v123", &norm, &masks);
        assert_eq!(out, format!("v{}", SENTINEL));
    }

    #[test]
    fn rect_mask_multirow() {
        let masks = vec![MaskRule::Rect(Rect::new(0, 0, 1, 2))];
        let out = apply_rect_masks_text("ab\ncd", &masks);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with(SENTINEL));
        assert!(lines[1].starts_with(SENTINEL));
    }
}
