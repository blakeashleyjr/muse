//! Diff report types (§12).

use muse_core::grid::Rect;
use muse_core::style::CellStyle;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StyleDelta {
    pub rect: Rect,
    pub baseline: CellStyle,
    pub actual: CellStyle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffReport {
    pub summary: String,
    /// Unified line diff (text/styled).
    pub unified: Option<String>,
    /// Per-cell style differences (styled tier).
    pub style_deltas: Vec<StyleDelta>,
    /// Fraction of differing pixels (pixel tier).
    pub pixel_ratio: Option<f32>,
    /// Diff PNG bytes (pixel tier).
    #[serde(skip)]
    pub diff_png: Option<Vec<u8>>,
}

impl DiffReport {
    pub fn text(summary: impl Into<String>, unified: String) -> DiffReport {
        DiffReport {
            summary: summary.into(),
            unified: Some(unified),
            style_deltas: Vec::new(),
            pixel_ratio: None,
            diff_png: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DiffResult {
    Match,
    Mismatch { report: DiffReport },
}

impl DiffResult {
    pub fn is_match(&self) -> bool {
        matches!(self, DiffResult::Match)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_match() {
        assert!(DiffResult::Match.is_match());
        let m = DiffResult::Mismatch {
            report: DiffReport::text("x", "diff".into()),
        };
        assert!(!m.is_match());
    }

    #[test]
    fn report_helper() {
        let r = DiffReport::text("summary", "u".into());
        assert_eq!(r.unified.as_deref(), Some("u"));
        assert!(r.style_deltas.is_empty());
    }
}
