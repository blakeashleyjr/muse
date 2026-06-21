//! Execute a spec case against the engine (§19).

use crate::report::{AssertionResult, CaseResult, SnapshotResult};
use crate::spec::{parse_color, Spec, Step};
use muse_core::config::SyncConfig;
use muse_core::input::{Mods, MouseAction, MouseButton, MouseEvent};
use muse_core::locator::StylePredicate;
use muse_core::snapshot::Snapshot;
use muse_core::style::Attrs;
use muse_diff::{BaselineOutcome, Baselines, DiffOptions};
use muse_engine::assert;
use muse_engine::{resolve_profile, spawn_terminal, TerminalHandle};
use regex::Regex;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

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

/// Expand `{case_tmp}` in a string.
fn expand(s: &str, case_tmp: &str) -> String {
    s.replace("{case_tmp}", case_tmp)
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

    // Create a per-case temp directory if requested. It is dropped (deleted)
    // at the end of this function, after the process exits.
    let _case_tmpdir = if spec.case_tmp_env.is_some() {
        match tempfile::TempDir::new() {
            Ok(d) => Some(d),
            Err(e) => {
                result.error = Some(format!("tempdir: {e}"));
                return result;
            }
        }
    } else {
        None
    };
    let case_tmp = _case_tmpdir
        .as_ref()
        .map(|d| d.path().to_string_lossy().into_owned())
        .unwrap_or_default();

    // Expand {case_tmp} in env values and inject the case_tmp_env key.
    let mut env = spec
        .env
        .iter()
        .map(|(k, v)| (k.clone(), expand(v, &case_tmp)))
        .collect::<std::collections::HashMap<_, _>>();
    if let Some(key) = &spec.case_tmp_env {
        env.insert(key.clone(), case_tmp.clone());
    }

    let profile = match resolve_profile(profile_name) {
        Ok(p) => p,
        Err(e) => {
            result.error = Some(e.to_string());
            return result;
        }
    };

    // Build sync config: RunOpts base, overridden by spec-level sync block.
    let sync_cfg = {
        let mut cfg = opts.sync.clone();
        if let Some(s) = &spec.sync {
            if let Some(qw) = s.quiet_window_ms {
                cfg.quiet_window_ms = qw;
            }
            if let Some(ms) = s.max_settle_ms {
                cfg.max_settle_ms = ms;
            }
        }
        cfg
    };

    let handle = match spawn_terminal(profile, cols, rows, spec.spawn.clone(), env, None, sync_cfg)
    {
        Ok(h) => h,
        Err(e) => {
            result.error = Some(e.to_string());
            return result;
        }
    };

    if let Err(e) = run_steps(spec, &handle, cols, rows, opts, &mut result, &case_tmp).await {
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
    case_tmp: &str,
) -> Result<(), String> {
    let store = Baselines::new(&opts.snapshots_dir, opts.update_snapshots);
    let dl = opts.assert_deadline_ms;
    // Per-path byte cursor for watch_log steps.
    let mut log_cursors: HashMap<String, u64> = HashMap::new();
    for step in &spec.steps {
        match step {
            Step::Write(s) => handle.write(s.clone().into_bytes()).await.map_err(es)?,
            Step::Paste(s) => handle.paste(s.clone().into_bytes()).await.map_err(es)?,
            Step::WriteLine(s) => {
                let mut bytes = s.clone().into_bytes();
                bytes.push(b'\n');
                handle.write(bytes).await.map_err(es)?;
            }
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
                let step_dl = loc.timeout_ms.unwrap_or(dl);
                let (l, ml) = loc.to_locator().map_err(es)?;
                let o = assert::to_be_visible(handle, l, ml, step_dl)
                    .await
                    .map_err(es)?;
                result.assertions.push(AssertionResult {
                    kind: "toBeVisible".into(),
                    ok: o.ok,
                    detail: o.detail,
                });
            }
            Step::ExpectNotVisible(loc) => {
                let step_dl = loc.timeout_ms.unwrap_or(dl);
                let (l, ml) = loc.to_locator().map_err(es)?;
                let o = assert::to_not_be_visible(handle, l, ml, step_dl)
                    .await
                    .map_err(es)?;
                result.assertions.push(AssertionResult {
                    kind: "toNotBeVisible".into(),
                    ok: o.ok,
                    detail: o.detail,
                });
            }
            Step::ExpectText(et) => {
                let step_dl = et.loc.timeout_ms.unwrap_or(dl);
                let (l, ml) = et.loc.to_locator().map_err(es)?;
                let o = assert::to_have_text(handle, l, &et.equals, ml, step_dl)
                    .await
                    .map_err(es)?;
                result.assertions.push(AssertionResult {
                    kind: "toHaveText".into(),
                    ok: o.ok,
                    detail: format!("expected={:?} actual={:?}", o.expected, o.actual),
                });
            }
            Step::ExpectContains(ec) => {
                let step_dl = ec.loc.timeout_ms.unwrap_or(dl);
                let (l, ml) = ec.loc.to_locator().map_err(es)?;
                let o = assert::to_contain_text(handle, l, &ec.contains, ml, step_dl)
                    .await
                    .map_err(es)?;
                result.assertions.push(AssertionResult {
                    kind: "toContainText".into(),
                    ok: o.ok,
                    detail: format!("expected={:?} actual={:?}", o.expected, o.actual),
                });
            }
            Step::ExpectCount(ec) => {
                let step_dl = ec.loc.timeout_ms.unwrap_or(dl);
                let (l, ml) = ec.loc.to_locator().map_err(es)?;
                let o = assert::to_have_count(handle, l, ec.eq, ec.min, ec.max, ml, step_dl)
                    .await
                    .map_err(es)?;
                result.assertions.push(AssertionResult {
                    kind: "toHaveCount".into(),
                    ok: o.ok,
                    detail: o.detail,
                });
            }
            Step::Snapshot(snap) => {
                let defaults = spec.snapshot_defaults.as_ref();
                let kind = snap.snapshot_kind(defaults.and_then(|d| d.kind.as_deref()));
                let mut masks = snap.mask_rules();
                if let Some(d) = defaults {
                    masks.extend(d.mask_rules());
                }
                let mut normalize = snap.normalize_rules();
                if let Some(d) = defaults {
                    normalize.extend(d.normalize_rules());
                }
                let s = handle.snapshot(kind, 1, dl).await.map_err(es)?;
                let diff_opts = DiffOptions {
                    masks,
                    normalize,
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
            Step::CheckFile(cf) => {
                let path = expand(&cf.path, case_tmp);
                let re = Regex::new(&cf.reject_re)
                    .map_err(|e| format!("check_file: bad regex {:?}: {e}", cf.reject_re))?;
                let ok = match std::fs::read_to_string(&path) {
                    Err(_) if cf.skip_if_missing => true,
                    Err(e) => {
                        result.assertions.push(AssertionResult {
                            kind: "checkFile".into(),
                            ok: false,
                            detail: format!("cannot read {path}: {e}"),
                        });
                        continue;
                    }
                    Ok(content) => {
                        let violations: Vec<&str> =
                            content.lines().filter(|l| re.is_match(l)).collect();
                        if violations.is_empty() {
                            true
                        } else {
                            result.assertions.push(AssertionResult {
                                kind: "checkFile".into(),
                                ok: false,
                                detail: format!(
                                    "log violations in {path}:\n{}",
                                    violations.join("\n")
                                ),
                            });
                            continue;
                        }
                    }
                };
                result.assertions.push(AssertionResult {
                    kind: "checkFile".into(),
                    ok,
                    detail: String::new(),
                });
            }
            Step::ExpectExit(ee) => {
                let got = handle.wait_exit(ee.timeout_ms).await.map_err(es)?;
                let ok = got.map(|c| c as i32 == ee.code).unwrap_or(false);
                result.assertions.push(AssertionResult {
                    kind: "exitCode".into(),
                    ok,
                    detail: format!("expected={} actual={:?}", ee.code, got),
                });
            }
            Step::ExpectStyle(es_spec) => {
                let step_dl = es_spec.loc.timeout_ms.unwrap_or(dl);
                let (l, ml) = es_spec.loc.to_locator().map_err(es)?;
                let mut attrs_all = Attrs::empty();
                if es_spec.bold == Some(true) {
                    attrs_all |= Attrs::BOLD;
                }
                if es_spec.italic == Some(true) {
                    attrs_all |= Attrs::ITALIC;
                }
                if es_spec.dim == Some(true) {
                    attrs_all |= Attrs::DIM;
                }
                if es_spec.underline == Some(true) {
                    attrs_all |= Attrs::UNDERLINE;
                }
                if es_spec.strike == Some(true) {
                    attrs_all |= Attrs::STRIKE;
                }
                if es_spec.reverse == Some(true) {
                    attrs_all |= Attrs::REVERSE;
                }
                let fg = es_spec
                    .fg
                    .as_deref()
                    .map(parse_color)
                    .transpose()
                    .map_err(es)?;
                let bg = es_spec
                    .bg
                    .as_deref()
                    .map(parse_color)
                    .transpose()
                    .map_err(es)?;
                let pred = StylePredicate {
                    fg,
                    bg,
                    attrs_all,
                    ..Default::default()
                };
                let o = assert::to_have_style(handle, l, pred, ml, step_dl)
                    .await
                    .map_err(es)?;
                result.assertions.push(AssertionResult {
                    kind: "toHaveStyle".into(),
                    ok: o.ok,
                    detail: o.detail,
                });
            }
            Step::Mouse(ms) => {
                let button = match ms.button.to_lowercase().as_str() {
                    "right" => MouseButton::Right,
                    "middle" => MouseButton::Middle,
                    "wheel_up" | "wheelup" => MouseButton::WheelUp,
                    "wheel_down" | "wheeldown" => MouseButton::WheelDown,
                    _ => MouseButton::Left,
                };
                let action = match ms.action.to_lowercase().as_str() {
                    "release" => MouseAction::Release,
                    "move" => MouseAction::Move,
                    _ => MouseAction::Press,
                };
                let mut mods = Mods::empty();
                for m in &ms.mods {
                    match m.to_lowercase().as_str() {
                        "ctrl" | "control" => mods |= Mods::CTRL,
                        "alt" | "meta" => mods |= Mods::ALT,
                        "shift" => mods |= Mods::SHIFT,
                        _ => {}
                    }
                }
                let ev = MouseEvent {
                    button,
                    action,
                    row: ms.row,
                    col: ms.col,
                    mods,
                };
                handle.mouse(ev).await.map_err(es)?;
            }
            Step::BeginStep(name) => {
                handle.end_step().await.ok();
                handle.begin_step(name.clone()).await.map_err(es)?;
            }
            Step::WatchLog(wl) => {
                let path = expand(&wl.path, case_tmp);
                let re = Regex::new(&wl.reject_re)
                    .map_err(|e| format!("watch_log: bad regex {:?}: {e}", wl.reject_re))?;
                let ok = match std::fs::File::open(&path) {
                    Err(_) if wl.skip_if_missing => true,
                    Err(e) => {
                        result.assertions.push(AssertionResult {
                            kind: "watchLog".into(),
                            ok: false,
                            detail: format!("cannot open {path}: {e}"),
                        });
                        continue;
                    }
                    Ok(mut f) => {
                        // Seek to the cursor position, read only new content.
                        let cursor = log_cursors.get(&path).copied().unwrap_or(0);
                        let _ = f.seek(SeekFrom::Start(cursor));
                        let mut new_content = String::new();
                        let _ = f.read_to_string(&mut new_content);
                        // Advance cursor by bytes consumed.
                        let new_cursor = cursor + new_content.len() as u64;
                        log_cursors.insert(path.clone(), new_cursor);
                        let violations: Vec<&str> =
                            new_content.lines().filter(|l| re.is_match(l)).collect();
                        if violations.is_empty() {
                            true
                        } else {
                            result.assertions.push(AssertionResult {
                                kind: "watchLog".into(),
                                ok: false,
                                detail: format!(
                                    "new log violations in {path}:\n{}",
                                    violations.join("\n")
                                ),
                            });
                            continue;
                        }
                    }
                };
                result.assertions.push(AssertionResult {
                    kind: "watchLog".into(),
                    ok,
                    detail: String::new(),
                });
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
    async fn snapshot_defaults_applied() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::from_yaml(
            r#"
name: snapdef
spawn: ["echo", "hello"]
snapshot_defaults:
  kind: styled
steps:
  - snapshot: {name: out}
"#,
        )
        .unwrap();
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(r.passed(), "{r:?}");
        assert_eq!(r.snapshots.len(), 1);
        // styled baseline is written (key contains #styled)
        let store = muse_diff::Baselines::new(dir.path(), false);
        let p = store.path("snapdef__out#styled", "xterm", 40, 10, "txt");
        assert!(p.exists(), "styled baseline should exist at {p:?}");
    }

    #[tokio::test]
    async fn snapshot_mismatch_reported() {
        let dir = tempfile::tempdir().unwrap();
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

    #[tokio::test]
    async fn expect_not_visible_passes_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::from_yaml(
            r#"
name: notvis
spawn: ["echo", "present"]
steps:
  - expect_not_visible: {text: "TOTALLY_ABSENT", timeout_ms: 400}
"#,
        )
        .unwrap();
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(r.passed(), "{r:?}");
        assert!(r.assertions[0].ok);
    }

    #[tokio::test]
    async fn expect_count_exact() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::from_yaml(
            r#"
name: cnt
spawn: ["sh", "-c", "printf 'x\nx\nx\n'"]
steps:
  - expect_count: {text: "x", eq: 3, timeout_ms: 2000}
"#,
        )
        .unwrap();
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(r.passed(), "{r:?}");
        assert!(r.assertions[0].ok);
    }

    #[tokio::test]
    async fn per_step_timeout_ms() {
        let dir = tempfile::tempdir().unwrap();
        // Short timeout per step; assertion must complete before global deadline
        let spec = Spec::from_yaml(
            r#"
name: pst
spawn: ["echo", "here"]
steps:
  - expect_visible: {text: "here", timeout_ms: 3000}
"#,
        )
        .unwrap();
        let mut o = opts(dir.path());
        o.assert_deadline_ms = 100; // very short global
        let r = run_case(&spec, "xterm", 40, 10, &o).await;
        // per-step overrides to 3000ms → should still pass
        assert!(r.passed(), "{r:?}");
    }

    #[tokio::test]
    async fn spec_sync_config_applied() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::from_yaml(
            r#"
name: synccfg
spawn: ["echo", "ok"]
sync:
  quiet_window_ms: 20
  max_settle_ms: 1000
steps:
  - expect_visible: {text: "ok"}
"#,
        )
        .unwrap();
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(r.passed(), "{r:?}");
    }

    #[tokio::test]
    async fn write_line_step() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::from_yaml(
            r#"
name: wl
spawn: ["cat"]
steps:
  - write_line: "hello-line"
  - expect_contains: {text: "hello-line", contains: "hello-line"}
"#,
        )
        .unwrap();
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(r.passed(), "{r:?}");
    }
}
