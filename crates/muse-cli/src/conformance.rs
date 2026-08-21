//! `muse conformance` — run emulator and protocol corpora from a directory.

use muse_runner::conformance::{parse_emulator_case, run_emulator_case};
use muse_runner::run::{run_case, RunOpts};
use muse_runner::spec::Spec;
use std::path::Path;

#[derive(Debug, Default)]
pub struct ConfSummary {
    pub passed: usize,
    pub failed: usize,
    pub details: Vec<(String, bool, String)>,
}

impl ConfSummary {
    /// Green only if something actually ran: an empty corpus directory is a
    /// misconfiguration, not a pass.
    pub fn ok(&self) -> bool {
        self.failed == 0 && self.passed > 0
    }
    pub fn render(&self) -> String {
        let mut s = String::new();
        for (name, ok, detail) in &self.details {
            s.push_str(&format!(
                "[{}] {}{}\n",
                if *ok { "PASS" } else { "FAIL" },
                name,
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(" — {detail}")
                }
            ));
        }
        s.push_str(&format!(
            "\n{} passed, {} failed\n",
            self.passed, self.failed
        ));
        s
    }
}

/// Every `*.yaml` under `dir`, recursively — the shipped corpora live in
/// `conformance/{emulator,protocol}/`, so the documented `muse conformance
/// conformance` invocation must descend.
fn yaml_files(dir: &Path) -> Vec<std::path::PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().and_then(|s| s.to_str()) == Some("yaml") {
                    out.push(p);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// Run every `*.yaml` in `dir`, dispatching emulator vs protocol corpus.
pub async fn run_dir(dir: &Path, snapshots_dir: &str) -> ConfSummary {
    let mut summary = ConfSummary::default();
    for path in yaml_files(dir) {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                summary.failed += 1;
                summary
                    .details
                    .push((path.display().to_string(), false, e.to_string()));
                continue;
            }
        };
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();

        // Try emulator corpus first (requires `feed`).
        if let Ok(case) = parse_emulator_case(&text) {
            let (ok, detail) = match run_emulator_case(&case) {
                Ok(()) => (true, String::new()),
                Err(e) => (false, e),
            };
            record(&mut summary, format!("emulator:{name}"), ok, detail);
            continue;
        }
        // Otherwise a protocol spec.
        match Spec::from_yaml(&text) {
            Ok(spec) => {
                let opts = RunOpts {
                    assert_deadline_ms: 3000,
                    snapshots_dir: snapshots_dir.to_string(),
                    artifacts_dir: None,
                    ..Default::default()
                };
                let profiles = spec.matrix.profiles_or_default();
                let sizes = spec.matrix.sizes_or_default();
                let (profile, (cols, rows)) = (
                    profiles[0].clone(),
                    sizes.first().copied().unwrap_or((80, 24)),
                );
                let r = run_case(&spec, &profile, cols, rows, &opts).await;
                let detail = if r.passed() {
                    String::new()
                } else {
                    r.error
                        .clone()
                        .unwrap_or_else(|| "assertion(s) failed".into())
                };
                record(&mut summary, format!("protocol:{name}"), r.passed(), detail);
            }
            Err(e) => record(&mut summary, name, false, e.to_string()),
        }
    }
    summary
}

fn record(s: &mut ConfSummary, name: String, ok: bool, detail: String) {
    if ok {
        s.passed += 1;
    } else {
        s.failed += 1;
    }
    s.details.push((name, ok, detail));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_emulator_and_protocol() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a_sgr.yaml"),
            "name: sgr\nfeed: \"\\e[31mHI\"\nexpect:\n  lines:\n    - text: \"HI\"\n      styles:\n        - {start: 0, len: 2, fg: {indexed: 1}}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b_echo.yaml"),
            "name: echo\nspawn: [\"echo\", \"conf-hi\"]\nsteps:\n  - expect_visible: {text: \"conf-hi\"}\n",
        )
        .unwrap();
        let snaps = tempfile::tempdir().unwrap();
        let sum = run_dir(dir.path(), &snaps.path().to_string_lossy()).await;
        assert_eq!(sum.passed, 2, "{}", sum.render());
        assert!(sum.ok());
    }

    #[tokio::test]
    async fn reports_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("bad.yaml"),
            "name: bad\nfeed: \"HI\"\nexpect:\n  lines:\n    - text: \"BYE\"\n",
        )
        .unwrap();
        let snaps = tempfile::tempdir().unwrap();
        let sum = run_dir(dir.path(), &snaps.path().to_string_lossy()).await;
        assert_eq!(sum.failed, 1);
        assert!(!sum.ok());
        assert!(sum.render().contains("[FAIL]"));
    }

    #[tokio::test]
    async fn empty_dir_is_not_ok() {
        // An empty corpus must not read as green.
        let dir = tempfile::tempdir().unwrap();
        let sum = run_dir(dir.path(), "snaps").await;
        assert!(!sum.ok());
        assert_eq!(sum.passed, 0);
    }

    #[tokio::test]
    async fn corpus_root_is_walked_recursively() {
        // `muse conformance conformance` (the documented invocation) must find
        // the cases under conformance/{emulator,protocol}/.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance");
        let snaps = tempfile::tempdir().unwrap();
        let sum = run_dir(&root, snaps.path().to_str().unwrap()).await;
        assert!(sum.ok(), "{}", sum.render());
        assert!(sum.passed >= 7);
    }

    #[tokio::test]
    async fn unparseable_yaml_records_failure() {
        let dir = tempfile::tempdir().unwrap();
        // neither a valid emulator case nor a valid protocol spec
        std::fs::write(dir.path().join("junk.yaml"), "this: : : not valid").unwrap();
        let snaps = tempfile::tempdir().unwrap();
        let sum = run_dir(dir.path(), &snaps.path().to_string_lossy()).await;
        assert_eq!(sum.failed, 1);
        assert!(!sum.ok());
    }

    #[tokio::test]
    async fn protocol_assertion_failure_detailed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("fail.yaml"),
            "name: f\nspawn: [\"echo\", \"x\"]\nsteps:\n  - expect_visible: {text: \"NEVER\"}\n",
        )
        .unwrap();
        let snaps = tempfile::tempdir().unwrap();
        let sum = run_dir(dir.path(), &snaps.path().to_string_lossy()).await;
        assert_eq!(sum.failed, 1);
        assert!(sum.render().contains("protocol:fail"));
    }

    #[tokio::test]
    async fn non_yaml_files_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "ignored").unwrap();
        let sum = run_dir(dir.path(), "snaps").await;
        assert_eq!(sum.passed + sum.failed, 0);
    }
}
