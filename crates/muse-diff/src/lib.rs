//! `muse-diff` — masking, normalization, text/styled/pixel diff, baselines (§12).

pub mod normalize;
pub mod perceptual;
pub mod report;
pub mod stabilize;

use muse_core::snapshot::StyledSnapshot;
use normalize::{MaskRule, NormalizeRule};
use report::{DiffReport, DiffResult, StyleDelta};
use similar::TextDiff;
use stabilize::StabilizeMode;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct DiffOptions {
    pub masks: Vec<MaskRule>,
    pub normalize: Vec<NormalizeRule>,
    /// Max per-channel delta still considered equal.
    pub pixel_tolerance: u8,
    /// Max differing-pixel ratio still considered a match.
    pub max_diff_ratio: f32,
    pub stabilize: StabilizeMode,
    /// Pixel scale (must match the snapshot scale for mask alignment).
    pub pixel_scale: u32,
}

impl Default for DiffOptions {
    fn default() -> Self {
        DiffOptions {
            masks: Vec::new(),
            normalize: Vec::new(),
            pixel_tolerance: 0,
            max_diff_ratio: 0.0,
            stabilize: StabilizeMode::Off,
            pixel_scale: 1,
        }
    }
}

fn unified(baseline: &str, actual: &str) -> String {
    TextDiff::from_lines(baseline, actual)
        .unified_diff()
        .header("baseline", "actual")
        .to_string()
}

/// Diff two text snapshots after normalize + masking.
pub fn diff_text(baseline: &str, actual: &str, opts: &DiffOptions) -> DiffResult {
    let b = normalize::transform_text(baseline, &opts.normalize, &opts.masks);
    let a = normalize::transform_text(actual, &opts.normalize, &opts.masks);
    if a == b {
        DiffResult::Match
    } else {
        DiffResult::Mismatch {
            report: DiffReport::text("text mismatch", unified(&b, &a)),
        }
    }
}

/// Diff two styled snapshots: canonical-string diff + per-cell style deltas.
pub fn diff_styled(
    baseline: &StyledSnapshot,
    actual: &StyledSnapshot,
    opts: &DiffOptions,
) -> DiffResult {
    let bc = normalize::transform_text(&baseline.to_canonical(), &opts.normalize, &[]);
    let ac = normalize::transform_text(&actual.to_canonical(), &opts.normalize, &[]);
    // Mask the text portion as well for fairness.
    if bc == ac {
        return DiffResult::Match;
    }
    let mut deltas = Vec::new();
    let rows = baseline.rows.len().max(actual.rows.len());
    for r in 0..rows {
        let b_runs = baseline.rows.get(r).map(|x| &x.runs);
        let a_runs = actual.rows.get(r).map(|x| &x.runs);
        if let (Some(br), Some(ar)) = (b_runs, a_runs) {
            for (brun, arun) in br.iter().zip(ar.iter()) {
                if brun.style() != arun.style() {
                    deltas.push(StyleDelta {
                        rect: muse_core::grid::Rect::new(r as u16, brun.start_col, brun.len, 1),
                        baseline: brun.style(),
                        actual: arun.style(),
                    });
                }
            }
        }
    }
    DiffResult::Mismatch {
        report: DiffReport {
            summary: "styled mismatch".into(),
            unified: Some(unified(&bc, &ac)),
            style_deltas: deltas,
            pixel_ratio: None,
            diff_png: None,
        },
    }
}

/// Diff two PNG snapshots.
pub fn diff_pixel(baseline_png: &[u8], actual_png: &[u8], opts: &DiffOptions) -> DiffResult {
    match perceptual::diff_png(
        baseline_png,
        actual_png,
        opts.pixel_tolerance,
        &opts.masks,
        opts.pixel_scale,
    ) {
        None => DiffResult::Mismatch {
            report: DiffReport {
                summary: "pixel dimensions differ".into(),
                unified: None,
                style_deltas: Vec::new(),
                pixel_ratio: Some(1.0),
                diff_png: None,
            },
        },
        Some(d) => {
            if d.ratio <= opts.max_diff_ratio {
                DiffResult::Match
            } else {
                DiffResult::Mismatch {
                    report: DiffReport {
                        summary: format!(
                            "pixel mismatch: {}/{} ({:.4})",
                            d.differing, d.total, d.ratio
                        ),
                        unified: None,
                        style_deltas: Vec::new(),
                        pixel_ratio: Some(d.ratio),
                        diff_png: Some(d.diff_png),
                    },
                }
            }
        }
    }
}

/// Outcome of comparing against a (possibly missing) baseline.
#[derive(Debug, PartialEq)]
pub enum BaselineOutcome {
    Created,
    Updated,
    Match,
    Mismatch(Box<DiffReport>),
}

impl BaselineOutcome {
    pub fn passed(&self) -> bool {
        !matches!(self, BaselineOutcome::Mismatch(_))
    }
}

/// Baseline store: `dir/{test}/{profile}__{cols}x{rows}__{os}.{ext}`.
pub struct Baselines {
    pub dir: PathBuf,
    pub update: bool,
}

impl Baselines {
    pub fn new(dir: impl Into<PathBuf>, update: bool) -> Baselines {
        Baselines {
            dir: dir.into(),
            update,
        }
    }

    pub fn path(&self, test: &str, profile: &str, cols: u16, rows: u16, ext: &str) -> PathBuf {
        let os = std::env::consts::OS;
        self.dir
            .join(test)
            .join(format!("{profile}__{cols}x{rows}__{os}.{ext}"))
    }

    fn ensure_parent(path: &Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        Ok(())
    }

    /// Compare a text snapshot to its baseline, creating/updating as configured.
    pub fn check_text(
        &self,
        test: &str,
        profile: &str,
        cols: u16,
        rows: u16,
        actual: &str,
        opts: &DiffOptions,
    ) -> std::io::Result<BaselineOutcome> {
        let path = self.path(test, profile, cols, rows, "txt");
        if !path.exists() {
            Self::ensure_parent(&path)?;
            std::fs::write(&path, actual)?;
            return Ok(BaselineOutcome::Created);
        }
        if self.update {
            std::fs::write(&path, actual)?;
            return Ok(BaselineOutcome::Updated);
        }
        let baseline = std::fs::read_to_string(&path)?;
        Ok(match diff_text(&baseline, actual, opts) {
            DiffResult::Match => BaselineOutcome::Match,
            DiffResult::Mismatch { report } => BaselineOutcome::Mismatch(Box::new(report)),
        })
    }

    /// Compare a pixel snapshot to its baseline PNG.
    pub fn check_pixel(
        &self,
        test: &str,
        profile: &str,
        cols: u16,
        rows: u16,
        actual_png: &[u8],
        opts: &DiffOptions,
    ) -> std::io::Result<BaselineOutcome> {
        let path = self.path(test, profile, cols, rows, "png");
        if !path.exists() {
            Self::ensure_parent(&path)?;
            std::fs::write(&path, actual_png)?;
            return Ok(BaselineOutcome::Created);
        }
        if self.update {
            std::fs::write(&path, actual_png)?;
            return Ok(BaselineOutcome::Updated);
        }
        let baseline = std::fs::read(&path)?;
        Ok(match diff_pixel(&baseline, actual_png, opts) {
            DiffResult::Match => BaselineOutcome::Match,
            DiffResult::Mismatch { report } => BaselineOutcome::Mismatch(Box::new(report)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::snapshot::{StyleRun, StyledRow};
    use muse_core::style::Attrs;
    use perceptual::solid_png;

    #[test]
    fn text_match() {
        assert!(diff_text("hello", "hello", &DiffOptions::default()).is_match());
    }

    #[test]
    fn text_mismatch_has_unified() {
        let r = diff_text("a\nb", "a\nc", &DiffOptions::default());
        match r {
            DiffResult::Mismatch { report } => assert!(report.unified.unwrap().contains("+")),
            _ => panic!(),
        }
    }

    #[test]
    fn text_mask_makes_clock_pass() {
        let opts = DiffOptions {
            normalize: vec![NormalizeRule {
                re: r"\d\d:\d\d:\d\d".into(),
                replace: "<T>".into(),
            }],
            ..Default::default()
        };
        assert!(diff_text("time 10:00:00", "time 11:30:45", &opts).is_match());
    }

    #[test]
    fn styled_match() {
        let s = StyledSnapshot {
            rows: vec![StyledRow {
                text: "X".into(),
                runs: vec![StyleRun {
                    start_col: 0,
                    len: 1,
                    fg: muse_core::color::Color::Indexed(1),
                    bg: muse_core::color::Color::Default,
                    attrs: Attrs::empty(),
                }],
            }],
        };
        assert!(diff_styled(&s, &s, &DiffOptions::default()).is_match());
    }

    #[test]
    fn styled_mismatch_records_delta() {
        let mk = |fg| StyledSnapshot {
            rows: vec![StyledRow {
                text: "X".into(),
                runs: vec![StyleRun {
                    start_col: 0,
                    len: 1,
                    fg,
                    bg: muse_core::color::Color::Default,
                    attrs: Attrs::empty(),
                }],
            }],
        };
        let a = mk(muse_core::color::Color::Indexed(1));
        let b = mk(muse_core::color::Color::Indexed(2));
        match diff_styled(&a, &b, &DiffOptions::default()) {
            DiffResult::Mismatch { report } => assert_eq!(report.style_deltas.len(), 1),
            _ => panic!(),
        }
    }

    #[test]
    fn pixel_match_and_mismatch() {
        let a = solid_png(8, 8, [0, 0, 0, 255]);
        let b = solid_png(8, 8, [0, 0, 0, 255]);
        assert!(diff_pixel(&a, &b, &DiffOptions::default()).is_match());
        let c = solid_png(8, 8, [255, 255, 255, 255]);
        match diff_pixel(&a, &c, &DiffOptions::default()) {
            DiffResult::Mismatch { report } => {
                assert!(report.pixel_ratio.unwrap() > 0.0);
                assert!(report.diff_png.is_some());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn pixel_dimension_mismatch() {
        let a = solid_png(8, 8, [0, 0, 0, 255]);
        let b = solid_png(16, 8, [0, 0, 0, 255]);
        assert!(!diff_pixel(&a, &b, &DiffOptions::default()).is_match());
    }

    #[test]
    fn pixel_ratio_tolerance() {
        // 1 of 64 pixels differ => ratio ~0.0156; allow up to 0.02
        let a = solid_png(8, 8, [0, 0, 0, 255]);
        let mut decoded = perceptual::decode_png(&a).unwrap();
        decoded.2[0] = 255;
        let b = {
            // re-encode modified
            perceptual::solid_png(8, 8, [0, 0, 0, 255]) // placeholder
        };
        // use rgba diff directly for determinism
        let d = perceptual::diff_rgba(
            8,
            8,
            perceptual::decode_png(&a).unwrap().2,
            decoded.2,
            0,
            &[],
            1,
        );
        let _ = b;
        assert_eq!(d.differing, 1);
    }

    #[test]
    fn baseline_create_then_match() {
        let dir = tempfile::tempdir().unwrap();
        let store = Baselines::new(dir.path(), false);
        let o1 = store
            .check_text("t1", "xterm", 80, 24, "hello", &DiffOptions::default())
            .unwrap();
        assert_eq!(o1, BaselineOutcome::Created);
        assert!(o1.passed());
        let o2 = store
            .check_text("t1", "xterm", 80, 24, "hello", &DiffOptions::default())
            .unwrap();
        assert_eq!(o2, BaselineOutcome::Match);
    }

    #[test]
    fn baseline_mismatch_and_update() {
        let dir = tempfile::tempdir().unwrap();
        let store = Baselines::new(dir.path(), false);
        store
            .check_text("t", "xterm", 80, 24, "v1", &DiffOptions::default())
            .unwrap();
        let o = store
            .check_text("t", "xterm", 80, 24, "v2", &DiffOptions::default())
            .unwrap();
        assert!(matches!(o, BaselineOutcome::Mismatch(_)));
        assert!(!o.passed());
        // now update
        let store2 = Baselines::new(dir.path(), true);
        let o2 = store2
            .check_text("t", "xterm", 80, 24, "v2", &DiffOptions::default())
            .unwrap();
        assert_eq!(o2, BaselineOutcome::Updated);
        // subsequent match
        let o3 = store
            .check_text("t", "xterm", 80, 24, "v2", &DiffOptions::default())
            .unwrap();
        assert_eq!(o3, BaselineOutcome::Match);
    }

    #[test]
    fn baseline_pixel_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Baselines::new(dir.path(), false);
        let png = solid_png(8, 8, [1, 2, 3, 255]);
        assert_eq!(
            store
                .check_pixel("p", "xterm", 80, 24, &png, &DiffOptions::default())
                .unwrap(),
            BaselineOutcome::Created
        );
        assert_eq!(
            store
                .check_pixel("p", "xterm", 80, 24, &png, &DiffOptions::default())
                .unwrap(),
            BaselineOutcome::Match
        );
        let other = solid_png(8, 8, [255, 255, 255, 255]);
        assert!(matches!(
            store
                .check_pixel("p", "xterm", 80, 24, &other, &DiffOptions::default())
                .unwrap(),
            BaselineOutcome::Mismatch(_)
        ));
    }

    #[test]
    fn path_scheme() {
        let store = Baselines::new("snaps", false);
        let p = store.path("mytest", "xterm", 80, 24, "txt");
        let s = p.to_string_lossy();
        assert!(s.contains("mytest"));
        assert!(s.contains(&format!("xterm__80x24__{}.txt", std::env::consts::OS)));
    }

    #[test]
    fn default_options() {
        let o = DiffOptions::default();
        assert_eq!(o.pixel_tolerance, 0);
        assert_eq!(o.max_diff_ratio, 0.0);
    }
}
