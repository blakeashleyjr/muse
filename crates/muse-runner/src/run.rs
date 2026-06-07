//! Execute a spec case against the engine (§19).

use crate::report::{AssertionResult, CaseResult, SnapshotResult};
use crate::spec::{Spec, Step};
use muse_core::config::SyncConfig;
use muse_core::snapshot::Snapshot;
use muse_diff::{BaselineOutcome, Baselines, DiffOptions};
use muse_engine::assert;
use muse_engine::{resolve_profile, spawn_terminal, TerminalHandle};

pub struct RunOpts {
    pub sync: SyncConfig,
    pub assert_deadline_ms: u64,
    pub snapshots_dir: String,
    pub update_snapshots: bool,
}

impl Default for RunOpts {
    fn default() -> Self {
        RunOpts {
            sync: SyncConfig::default(),
            assert_deadline_ms: 5000,
            snapshots_dir: "snapshots".into(),
            update_snapshots: false,
        }
    }
}

/// Run a single (profile × size) case.
pub async fn run_case(
    spec: &Spec,
    profile_name: &str,
    cols: u16,
    rows: u16,
    opts: &RunOpts,
) -> CaseResult {
    let mut result = CaseResult {
        name: spec.name.clone(),
        profile: profile_name.to_string(),
        cols,
        rows,
        assertions: Vec::new(),
        snapshots: Vec::new(),
        error: None,
        flaky: false,
    };

    let profile = match resolve_profile(profile_name) {
        Ok(p) => p,
        Err(e) => {
            result.error = Some(e.to_string());
            return result;
        }
    };

    let handle = match spawn_terminal(
        profile,
        cols,
        rows,
        spec.spawn.clone(),
        spec.env.clone(),
        None,
        opts.sync.clone(),
    ) {
        Ok(h) => h,
        Err(e) => {
            result.error = Some(e.to_string());
            return result;
        }
    };

    if let Err(e) = run_steps(spec, &handle, cols, rows, opts, &mut result).await {
        result.error = Some(e);
    }

    handle.shutdown().await.ok();
    result
}

async fn run_steps(
    spec: &Spec,
    handle: &TerminalHandle,
    cols: u16,
    rows: u16,
    opts: &RunOpts,
    result: &mut CaseResult,
) -> Result<(), String> {
    let store = Baselines::new(&opts.snapshots_dir, opts.update_snapshots);
    let dl = opts.assert_deadline_ms;
    for step in &spec.steps {
        match step {
            Step::Write(s) => handle.write(s.clone().into_bytes()).await.map_err(es)?,
            Step::Paste(s) => handle.paste(s.clone().into_bytes()).await.map_err(es)?,
            Step::Key(k) => {
                let ev = k.to_event().map_err(es)?;
                handle.key(ev).await.map_err(es)?;
            }
            Step::Resize(sz) => {
                let (c, r) =
                    crate::spec::parse_size(sz).ok_or_else(|| format!("bad size `{sz}`"))?;
                handle.resize(c, r).await.map_err(es)?;
            }
            Step::SleepMs(ms) => {
                tokio::time::sleep(std::time::Duration::from_millis(*ms)).await;
            }
            Step::ExpectVisible(loc) => {
                let (l, ml) = loc.to_locator().map_err(es)?;
                let o = assert::to_be_visible(handle, l, ml, dl).await.map_err(es)?;
                result.assertions.push(AssertionResult {
                    kind: "toBeVisible".into(),
                    ok: o.ok,
                    detail: o.detail,
                });
            }
            Step::ExpectText(et) => {
                let (l, ml) = et.loc.to_locator().map_err(es)?;
                let o = assert::to_have_text(handle, l, &et.equals, ml, dl)
                    .await
                    .map_err(es)?;
                result.assertions.push(AssertionResult {
                    kind: "toHaveText".into(),
                    ok: o.ok,
                    detail: format!("expected={:?} actual={:?}", o.expected, o.actual),
                });
            }
            Step::ExpectContains(ec) => {
                let (l, ml) = ec.loc.to_locator().map_err(es)?;
                let o = assert::to_contain_text(handle, l, &ec.contains, ml, dl)
                    .await
                    .map_err(es)?;
                result.assertions.push(AssertionResult {
                    kind: "toContainText".into(),
                    ok: o.ok,
                    detail: format!("expected={:?} actual={:?}", o.expected, o.actual),
                });
            }
            Step::Snapshot(snap) => {
                let kind = snap.snapshot_kind();
                let s = handle.snapshot(kind, 1, dl).await.map_err(es)?;
                let diff_opts = DiffOptions {
                    masks: snap.mask_rules(),
                    normalize: snap.normalize_rules(),
                    pixel_scale: snap.scale.max(1) as u32,
                    ..Default::default()
                };
                let test_key = format!("{}__{}", spec.name, snap.name);
                let outcome = check_snapshot(
                    &store,
                    &test_key,
                    &result.profile,
                    cols,
                    rows,
                    &s,
                    &diff_opts,
                )
                .map_err(|e| e.to_string())?;
                result.snapshots.push(snapshot_result(&snap.name, outcome));
            }
        }
    }
    Ok(())
}

fn check_snapshot(
    store: &Baselines,
    test_key: &str,
    profile: &str,
    cols: u16,
    rows: u16,
    snap: &Snapshot,
    opts: &DiffOptions,
) -> std::io::Result<BaselineOutcome> {
    match snap {
        Snapshot::Text(t) => store.check_text(test_key, profile, cols, rows, t, opts),
        Snapshot::Styled(s) => store.check_text(
            &format!("{test_key}#styled"),
            profile,
            cols,
            rows,
            &s.to_canonical(),
            opts,
        ),
        Snapshot::Pixel(p) => store.check_pixel(test_key, profile, cols, rows, &p.png, opts),
    }
}

fn snapshot_result(name: &str, outcome: BaselineOutcome) -> SnapshotResult {
    let (passed, text) = match &outcome {
        BaselineOutcome::Created => (true, "created".to_string()),
        BaselineOutcome::Updated => (true, "updated".to_string()),
        BaselineOutcome::Match => (true, "match".to_string()),
        BaselineOutcome::Mismatch(r) => (false, format!("mismatch: {}", r.summary)),
    };
    SnapshotResult {
        name: name.to_string(),
        outcome: text,
        passed,
    }
}

fn es<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(dir: &std::path::Path) -> RunOpts {
        RunOpts {
            assert_deadline_ms: 2000,
            snapshots_dir: dir.to_string_lossy().to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn all_step_kinds_and_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::from_yaml(
            r#"
name: full
spawn: ["cat"]
steps:
  - write: "line-one\n"
  - paste: "pasted\n"
  - key: {key: x}
  - key: {key: enter}
  - resize: "30x8"
  - sleep_ms: 20
  - expect_contains: {line: 0, contains: "line-one"}
  - snapshot: {name: txt, kind: text}
  - snapshot: {name: sty, kind: styled}
  - snapshot: {name: pix, kind: pixel, scale: 1}
"#,
        )
        .unwrap();
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(r.passed(), "{r:?}");
        assert_eq!(r.snapshots.len(), 3);
        assert!(r.snapshots.iter().all(|s| s.outcome == "created"));
    }

    #[tokio::test]
    async fn snapshot_mismatch_reported() {
        let dir = tempfile::tempdir().unwrap();
        // pre-create a baseline with wrong content
        let store = muse_diff::Baselines::new(dir.path(), false);
        let path = store.path("snapmis__out", "xterm", 40, 10, "txt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "TOTALLY DIFFERENT").unwrap();

        let spec = Spec::from_yaml(
            r#"
name: snapmis
spawn: ["echo", "real-output"]
steps:
  - snapshot: {name: out, kind: text}
"#,
        )
        .unwrap();
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(!r.passed());
        assert!(!r.snapshots[0].passed);
        assert!(r.snapshots[0].outcome.contains("mismatch"));
    }

    #[tokio::test]
    async fn update_snapshots_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let store = muse_diff::Baselines::new(dir.path(), false);
        let path = store.path("upd__out", "xterm", 40, 10, "txt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "old").unwrap();
        let spec = Spec::from_yaml(
            "name: upd\nspawn: [\"echo\", \"new-content\"]\nsteps:\n  - snapshot: {name: out, kind: text}\n",
        )
        .unwrap();
        let mut o = opts(dir.path());
        o.update_snapshots = true;
        let r = run_case(&spec, "xterm", 40, 10, &o).await;
        assert!(r.passed());
        assert_eq!(r.snapshots[0].outcome, "updated");
    }

    #[tokio::test]
    async fn bad_key_step_errors() {
        let dir = tempfile::tempdir().unwrap();
        let spec =
            Spec::from_yaml("name: bk\nspawn: [\"cat\"]\nsteps:\n  - key: {key: boguskey}\n")
                .unwrap();
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(r.error.is_some());
    }

    #[tokio::test]
    async fn bad_resize_step_errors() {
        let dir = tempfile::tempdir().unwrap();
        let spec =
            Spec::from_yaml("name: br\nspawn: [\"cat\"]\nsteps:\n  - resize: \"notasize\"\n")
                .unwrap();
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(r.error.is_some());
    }

    #[tokio::test]
    async fn run_echo_case_passes() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::from_yaml(
            r#"
name: echo
spawn: ["echo", "runner-hi"]
steps:
  - expect_visible: {text: "runner-hi"}
  - expect_text: {line: 0, equals: "runner-hi"}
  - snapshot: {name: out, kind: text}
"#,
        )
        .unwrap();
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(r.passed(), "{r:?}");
        assert_eq!(r.assertions.len(), 2);
        assert_eq!(r.snapshots.len(), 1);
        // snapshot created on first run
        assert_eq!(r.snapshots[0].outcome, "created");
    }

    #[tokio::test]
    async fn failing_assertion_marks_fail() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::from_yaml(
            r#"
name: nope
spawn: ["echo", "x"]
steps:
  - expect_visible: {text: "NOTHERE"}
"#,
        )
        .unwrap();
        let mut o = opts(dir.path());
        o.assert_deadline_ms = 300;
        let r = run_case(&spec, "xterm", 40, 10, &o).await;
        assert!(!r.passed());
        assert!(!r.assertions[0].ok);
    }

    #[tokio::test]
    async fn bad_profile_errors() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::from_yaml("name: x\nspawn: [echo]\n").unwrap();
        let r = run_case(&spec, "bogus", 40, 10, &opts(dir.path())).await;
        assert!(r.error.is_some());
        assert!(!r.passed());
    }

    #[tokio::test]
    async fn snapshot_match_on_second_run() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::from_yaml(
            r#"
name: snap
spawn: ["echo", "stable"]
steps:
  - snapshot: {name: out, kind: styled}
"#,
        )
        .unwrap();
        let r1 = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert_eq!(r1.snapshots[0].outcome, "created");
        let r2 = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert_eq!(r2.snapshots[0].outcome, "match");
    }
}
