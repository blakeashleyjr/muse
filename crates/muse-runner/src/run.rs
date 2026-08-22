//! Execute a spec case against the engine (§19).

use crate::report::{AssertionResult, CaseResult, SnapshotResult};
use crate::spec::{parse_color, Spec, Step};
use muse_core::config::SyncConfig;
use muse_core::locator::StylePredicate;
use muse_core::snapshot::Snapshot;
use muse_core::style::Attrs;
use muse_diff::{BaselineOutcome, Baselines, DiffOptions};
use muse_engine::assert;
use muse_engine::{resolve_profile, spawn_terminal, TerminalHandle};
use muse_trace::TraceMeta;
use regex::Regex;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// When to keep a per-case trace directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TraceMode {
    /// Never record.
    Off,
    /// Record every case; keep the artifacts only for failing ones.
    #[default]
    RetainOnFailure,
    /// Record and keep for every case.
    On,
}

impl TraceMode {
    pub fn parse(s: &str) -> Option<TraceMode> {
        match s {
            "off" => Some(TraceMode::Off),
            "retain-on-failure" => Some(TraceMode::RetainOnFailure),
            "on" => Some(TraceMode::On),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct RunOpts {
    pub sync: SyncConfig,
    pub assert_deadline_ms: u64,
    pub snapshots_dir: String,
    pub update_snapshots: bool,
    /// Refuse to create missing baselines (`--ci`): a snapshot with no
    /// baseline is a failure, not a silent pass.
    pub forbid_create: bool,
    /// Where per-case failure artifacts go (`test-results` by default);
    /// `None` disables artifacts entirely.
    pub artifacts_dir: Option<PathBuf>,
    pub trace: TraceMode,
    /// Normalize rules applied to every snapshot (from `[[normalize]]` in
    /// muse.toml), after the spec's own rules.
    pub default_normalize: Vec<muse_diff::normalize::NormalizeRule>,
}

impl Default for RunOpts {
    fn default() -> Self {
        RunOpts {
            sync: SyncConfig::default(),
            assert_deadline_ms: 5000,
            snapshots_dir: "snapshots".into(),
            update_snapshots: false,
            forbid_create: false,
            artifacts_dir: Some(PathBuf::from("test-results")),
            trace: TraceMode::RetainOnFailure,
            default_normalize: Vec::new(),
        }
    }
}

/// `name__profile__WxH` with anything path-hostile replaced.
pub fn case_slug(name: &str, profile: &str, cols: u16, rows: u16) -> String {
    let raw = format!("{name}__{profile}__{cols}x{rows}");
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Write the final screen (text + png + cursor/title/modes) and the result
/// record into `dir`. Best-effort: a failure to write an artifact must never
/// change the verdict, so errors are folded into `result.error` only when the
/// case had no error yet.
async fn write_final_artifacts(handle: &TerminalHandle, dir: &Path, result: &CaseResult) {
    if let Ok(Snapshot::Text(t)) = handle
        .snapshot(muse_core::snapshot::SnapshotKind::Text, 1, 500)
        .await
    {
        let _ = std::fs::write(dir.join("final.txt"), t);
    }
    if let Ok(Snapshot::Pixel(p)) = handle
        .snapshot(
            muse_core::snapshot::SnapshotKind::Pixel { scale: 1 },
            1,
            500,
        )
        .await
    {
        let _ = std::fs::write(dir.join("final.png"), p.png);
    }
    if let Ok((screen, generation)) = handle.screen().await {
        let info = serde_json::json!({
            "generation": generation,
            "cursor": screen.cursor,
            "title": screen.title,
            "modes": screen.modes,
            "alt_screen": screen.active == muse_core::screen::ScreenKind::Alt,
            "cols": screen.active_grid().cols(),
            "rows": screen.active_grid().rows(),
        });
        let _ = std::fs::write(
            dir.join("final.json"),
            serde_json::to_string_pretty(&info).unwrap_or_default(),
        );
    }
    let _ = std::fs::write(
        dir.join("result.json"),
        serde_json::to_string_pretty(result).unwrap_or_default(),
    );
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
        artifacts: None,
        duration_ms: 0,
    };
    let t0 = std::time::Instant::now();

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

    let env_for_meta: Vec<(String, String)> =
        env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let profile_label = profile.name.clone();
    let handle = match spawn_terminal(profile, cols, rows, spec.spawn.clone(), env, None, sync_cfg)
    {
        Ok(h) => h,
        Err(e) => {
            result.error = Some(e.to_string());
            result.duration_ms = t0.elapsed().as_millis() as u64;
            return result;
        }
    };

    // Per-case artifact directory: wiped first so a rerun never shows stale
    // files from a previous verdict.
    let case_dir = opts
        .artifacts_dir
        .as_ref()
        .map(|d| d.join(case_slug(&spec.name, profile_name, cols, rows)));
    if let Some(dir) = &case_dir {
        let _ = std::fs::remove_dir_all(dir);
        if let Err(e) = std::fs::create_dir_all(dir) {
            result.error = Some(format!("artifacts dir {}: {e}", dir.display()));
            handle.shutdown().await.ok();
            result.duration_ms = t0.elapsed().as_millis() as u64;
            return result;
        }
        if opts.trace != TraceMode::Off {
            let meta = TraceMeta {
                version: 1,
                profile: profile_label,
                cols,
                rows,
                env: env_for_meta,
                started_at: unix_now(),
                sut_argv: spec.spawn.clone(),
            };
            let _ = handle.start_trace(dir.join("trace"), meta).await;
        }
    }

    let ctx = CaseCtx {
        cols,
        rows,
        case_tmp: case_tmp.clone(),
        case_dir: case_dir.clone(),
    };
    if let Err(e) = run_steps(spec, &handle, opts, &mut result, &ctx).await {
        result.error = Some(e);
    }

    result.duration_ms = t0.elapsed().as_millis() as u64;
    tracing::info!(
        case = %result.case_id(),
        passed = result.passed(),
        ms = result.duration_ms,
        "case finished"
    );
    if let Some(dir) = &case_dir {
        let keep = !result.passed() || opts.trace == TraceMode::On;
        if keep {
            if opts.trace != TraceMode::Off {
                let _ = handle.export_trace().await;
            }
            write_final_artifacts(&handle, dir, &result).await;
            result.artifacts = Some(dir.to_string_lossy().into_owned());
        } else {
            // The actor's shutdown flushes an active trace — discard it first
            // or the flush recreates `trace/` after this removal.
            let _ = handle.discard_trace().await;
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    handle.shutdown().await.ok();
    result
}

/// Per-case facts the step loop needs.
struct CaseCtx {
    cols: u16,
    rows: u16,
    case_tmp: String,
    case_dir: Option<PathBuf>,
}

async fn run_steps(
    spec: &Spec,
    handle: &TerminalHandle,
    opts: &RunOpts,
    result: &mut CaseResult,
    ctx: &CaseCtx,
) -> Result<(), String> {
    let CaseCtx { cols, rows, .. } = *ctx;
    let case_tmp = ctx.case_tmp.as_str();
    let case_dir = ctx.case_dir.as_deref();
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
                push_assertion(handle, result, "toBeVisible", o.ok, o.detail).await;
            }
            Step::ExpectNotVisible(loc) => {
                let step_dl = loc.timeout_ms.unwrap_or(dl);
                let (l, ml) = loc.to_locator().map_err(es)?;
                let o = assert::to_not_be_visible(handle, l, ml, step_dl)
                    .await
                    .map_err(es)?;
                push_assertion(handle, result, "toNotBeVisible", o.ok, o.detail).await;
            }
            Step::ExpectText(et) => {
                let step_dl = et.loc.timeout_ms.unwrap_or(dl);
                let (l, ml) = et.loc.to_locator().map_err(es)?;
                let o = assert::to_have_text(handle, l, &et.equals, ml, step_dl)
                    .await
                    .map_err(es)?;
                push_assertion(
                    handle,
                    result,
                    "toHaveText",
                    o.ok,
                    format!("expected={:?} actual={:?}", o.expected, o.actual),
                )
                .await;
            }
            Step::ExpectContains(ec) => {
                let step_dl = ec.loc.timeout_ms.unwrap_or(dl);
                let (l, ml) = ec.loc.to_locator().map_err(es)?;
                let o = assert::to_contain_text(handle, l, &ec.contains, ml, step_dl)
                    .await
                    .map_err(es)?;
                push_assertion(
                    handle,
                    result,
                    "toContainText",
                    o.ok,
                    format!("expected={:?} actual={:?}", o.expected, o.actual),
                )
                .await;
            }
            Step::ExpectCount(ec) => {
                let step_dl = ec.loc.timeout_ms.unwrap_or(dl);
                let (l, ml) = ec.loc.to_locator().map_err(es)?;
                let o = assert::to_have_count(handle, l, ec.eq, ec.min, ec.max, ml, step_dl)
                    .await
                    .map_err(es)?;
                push_assertion(handle, result, "toHaveCount", o.ok, o.detail).await;
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
                normalize.extend(opts.default_normalize.iter().cloned());
                let s = handle.snapshot(kind, 1, dl).await.map_err(es)?;
                let diff_opts = DiffOptions {
                    masks,
                    normalize,
                    pixel_scale: snap.scale.max(1) as u32,
                    ..Default::default()
                };
                let test_key = format!("{}__{}", spec.name, snap.name);
                if opts.forbid_create
                    && !baseline_path(&store, &test_key, &result.profile, cols, rows, &s).exists()
                {
                    result.snapshots.push(SnapshotResult {
                        name: snap.name.clone(),
                        outcome: "missing baseline (creation forbidden by --ci)".into(),
                        passed: false,
                    });
                    if let Some(dir) = case_dir {
                        write_actual(dir, &snap.name, &s);
                    }
                    continue;
                }
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
                if let (Some(dir), BaselineOutcome::Mismatch(report)) = (case_dir, &outcome) {
                    write_actual(dir, &snap.name, &s);
                    if let Some(u) = &report.unified {
                        let _ = std::fs::write(dir.join(format!("{}.diff.txt", snap.name)), u);
                    }
                    if let Some(png) = &report.diff_png {
                        let _ = std::fs::write(dir.join(format!("{}.diff.png", snap.name)), png);
                    }
                    let bp = baseline_path(&store, &test_key, &result.profile, cols, rows, &s);
                    let ext = bp.extension().and_then(|e| e.to_str()).unwrap_or("txt");
                    let _ = std::fs::copy(&bp, dir.join(format!("{}.baseline.{ext}", snap.name)));
                }
                result.snapshots.push(snapshot_result(&snap.name, outcome));
            }
            Step::CheckFile(cf) => {
                let path = expand(&cf.path, case_tmp);
                let re = Regex::new(&cf.reject_re)
                    .map_err(|e| format!("check_file: bad regex {:?}: {e}", cf.reject_re))?;
                let ok = match std::fs::read_to_string(&path) {
                    Err(_) if cf.skip_if_missing => true,
                    Err(e) => {
                        push_assertion(
                            handle,
                            result,
                            "checkFile",
                            false,
                            format!("cannot read {path}: {e}"),
                        )
                        .await;
                        continue;
                    }
                    Ok(content) => {
                        let violations: Vec<&str> =
                            content.lines().filter(|l| re.is_match(l)).collect();
                        if violations.is_empty() {
                            true
                        } else {
                            push_assertion(
                                handle,
                                result,
                                "checkFile",
                                false,
                                format!("log violations in {path}:\n{}", violations.join("\n")),
                            )
                            .await;
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
                push_assertion(handle, result, "toHaveStyle", o.ok, o.detail).await;
            }
            Step::Mouse(ms) => {
                let ev = ms.to_event().map_err(es)?;
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
                        push_assertion(
                            handle,
                            result,
                            "watchLog",
                            false,
                            format!("cannot open {path}: {e}"),
                        )
                        .await;
                        continue;
                    }
                    Ok(mut f) => {
                        // Seek to the cursor position, read only new content.
                        // Read bytes (a non-UTF-8 log must fail loudly, not read
                        // as empty), and only consume up to the last complete
                        // line so a line split across two steps is never
                        // matched in halves.
                        let cursor = log_cursors.get(&path).copied().unwrap_or(0);
                        f.seek(SeekFrom::Start(cursor))
                            .map_err(|e| format!("watch_log: seek {path}: {e}"))?;
                        let mut raw = Vec::new();
                        f.read_to_end(&mut raw)
                            .map_err(|e| format!("watch_log: read {path}: {e}"))?;
                        let consumed = raw.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
                        let new_content = String::from_utf8(raw[..consumed].to_vec())
                            .map_err(|e| format!("watch_log: {path} is not UTF-8: {e}"))?;
                        log_cursors.insert(path.clone(), cursor + consumed as u64);
                        let violations: Vec<&str> =
                            new_content.lines().filter(|l| re.is_match(l)).collect();
                        if violations.is_empty() {
                            true
                        } else {
                            push_assertion(
                                handle,
                                result,
                                "watchLog",
                                false,
                                format!("new log violations in {path}:\n{}", violations.join("\n")),
                            )
                            .await;
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

/// Where the baseline for this snapshot lives (text and styled share `.txt`).
fn baseline_path(
    store: &Baselines,
    test_key: &str,
    profile: &str,
    cols: u16,
    rows: u16,
    snap: &Snapshot,
) -> PathBuf {
    match snap {
        Snapshot::Text(_) => store.path(test_key, profile, cols, rows, "txt"),
        Snapshot::Styled(_) => {
            store.path(&format!("{test_key}#styled"), profile, cols, rows, "txt")
        }
        Snapshot::Pixel(_) => store.path(test_key, profile, cols, rows, "png"),
    }
}

/// Write the snapshot as taken into the case artifact dir.
fn write_actual(dir: &Path, name: &str, snap: &Snapshot) {
    match snap {
        Snapshot::Text(t) => {
            let _ = std::fs::write(dir.join(format!("{name}.actual.txt")), t);
        }
        Snapshot::Styled(s) => {
            let _ = std::fs::write(dir.join(format!("{name}.actual.txt")), s.to_canonical());
        }
        Snapshot::Pixel(p) => {
            let _ = std::fs::write(dir.join(format!("{name}.actual.png")), &p.png);
        }
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

/// Record an assertion on the case result and mirror it into the trace.
async fn push_assertion(
    handle: &TerminalHandle,
    result: &mut CaseResult,
    kind: impl Into<String>,
    ok: bool,
    detail: impl Into<String>,
) {
    let kind = kind.into();
    let detail = detail.into();
    let _ = handle
        .record_assertion(kind.clone(), ok, detail.clone())
        .await;
    result.assertions.push(AssertionResult { kind, ok, detail });
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
            artifacts_dir: None,
            ..Default::default()
        }
    }

    #[test]
    fn slug_is_path_safe() {
        assert_eq!(case_slug("a b/c", "xterm", 80, 24), "a_b_c__xterm__80x24");
        assert_eq!(TraceMode::parse("on"), Some(TraceMode::On));
        assert_eq!(TraceMode::parse("nope"), None);
    }

    #[tokio::test]
    async fn failing_case_writes_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let art = dir.path().join("results");
        let spec = Spec::from_yaml(
            "name: art\nspawn: [\"sh\", \"-c\", \"echo visible; exec cat\"]\nsteps:\n  - expect_visible: {text: \"visible\"}\n  - expect_visible: {text: \"absent\", timeout_ms: 200}\n",
        )
        .unwrap();
        let o = RunOpts {
            artifacts_dir: Some(art.clone()),
            ..opts(dir.path())
        };
        let r = run_case(&spec, "xterm", 40, 10, &o).await;
        assert!(!r.passed());
        let case = art.join("art__xterm__40x10");
        assert_eq!(r.artifacts.as_deref(), Some(case.to_str().unwrap()));
        for f in ["final.txt", "final.png", "final.json", "result.json"] {
            assert!(case.join(f).exists(), "missing {f}");
        }
        assert!(std::fs::read_to_string(case.join("final.txt"))
            .unwrap()
            .contains("visible"));
        assert!(case.join("trace/meta.json").exists());
        assert!(case.join("trace/output.cast").exists());
        assert!(r.duration_ms > 0);
    }

    #[tokio::test]
    async fn passing_case_cleans_artifacts_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let art = dir.path().join("results");
        let spec = Spec::from_yaml(
            "name: clean\nspawn: [\"echo\", \"hi\"]\nsteps:\n  - expect_visible: {text: \"hi\"}\n",
        )
        .unwrap();
        let o = RunOpts {
            artifacts_dir: Some(art.clone()),
            ..opts(dir.path())
        };
        let r = run_case(&spec, "xterm", 40, 10, &o).await;
        assert!(r.passed());
        assert!(r.artifacts.is_none());
        // give the actor's shutdown a moment: nothing may reappear
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!art.join("clean__xterm__40x10").exists());
        // trace=on keeps it
        let o = RunOpts {
            trace: TraceMode::On,
            ..o
        };
        let r = run_case(&spec, "xterm", 40, 10, &o).await;
        assert!(r.passed());
        assert!(art.join("clean__xterm__40x10/trace/meta.json").exists());
    }

    #[tokio::test]
    async fn snapshot_mismatch_writes_diff_files() {
        let dir = tempfile::tempdir().unwrap();
        let art = dir.path().join("results");
        let snaps = dir.path().join("snaps");
        let mk = |word: &str| {
            Spec::from_yaml(&format!(
                "name: mm\nspawn: [\"echo\", \"{word}\"]\nsteps:\n  - expect_visible: {{text: \"{word}\"}}\n  - snapshot: {{name: s, kind: text}}\n  - snapshot: {{name: p, kind: pixel}}\n"
            ))
            .unwrap()
        };
        let o = RunOpts {
            artifacts_dir: Some(art.clone()),
            snapshots_dir: snaps.to_string_lossy().to_string(),
            ..opts(dir.path())
        };
        assert!(run_case(&mk("one"), "xterm", 40, 10, &o).await.passed());
        let r = run_case(&mk("two"), "xterm", 40, 10, &o).await;
        assert!(!r.passed());
        let case = art.join("mm__xterm__40x10");
        for f in [
            "s.actual.txt",
            "s.diff.txt",
            "s.baseline.txt",
            "p.actual.png",
            "p.diff.png",
            "p.baseline.png",
        ] {
            assert!(case.join(f).exists(), "missing {f}");
        }
    }

    #[tokio::test]
    async fn ci_mode_refuses_to_create_baselines() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::from_yaml(
            "name: ci\nspawn: [\"echo\", \"hi\"]\nsteps:\n  - snapshot: {name: s, kind: text}\n",
        )
        .unwrap();
        let o = RunOpts {
            forbid_create: true,
            ..opts(dir.path())
        };
        let r = run_case(&spec, "xterm", 40, 10, &o).await;
        assert!(!r.passed());
        assert!(r.snapshots[0].outcome.contains("missing baseline"));
        // and nothing was written
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
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

    #[tokio::test]
    async fn assertions_are_recorded_in_the_trace() {
        let dir = tempfile::tempdir().unwrap();
        let art = dir.path().join("results");
        let spec = Spec::from_yaml(
            "name: tr\nspawn: [\"echo\", \"hi\"]\nsteps:\n  - begin_step: \"look\"\n  - expect_visible: {text: \"hi\"}\n  - expect_visible: {text: \"nope\", timeout_ms: 100}\n",
        )
        .unwrap();
        let o = RunOpts {
            artifacts_dir: Some(art.clone()),
            ..opts(dir.path())
        };
        let r = run_case(&spec, "xterm", 40, 10, &o).await;
        assert!(!r.passed());
        let steps =
            std::fs::read_to_string(art.join("tr__xterm__40x10/trace/steps.jsonl")).unwrap();
        assert!(steps.contains("\"look\""), "{steps}");
        assert!(steps.contains("toBeVisible"), "{steps}");
        assert!(steps.contains("\"ok\":false"), "{steps}");
    }

    #[tokio::test]
    async fn watch_log_does_not_split_lines_and_rejects_non_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "fine\nERR").unwrap(); // partial last line
        let spec_text = format!(
            "name: wl\nspawn: [\"cat\"]\nsteps:\n  - watch_log: {{path: \"{0}\", reject_re: \"ERROR\"}}\n  - sleep_ms: 50\n  - watch_log: {{path: \"{0}\", reject_re: \"ERROR\"}}\n",
            log.display()
        );
        let spec = Spec::from_yaml(&spec_text).unwrap();
        // Complete the line between the two watches: the violation spans the
        // point where the first watch stopped reading.
        let log2 = log.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log2)
                .unwrap();
            f.write_all(b"OR happened\n").unwrap();
        });
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(!r.passed(), "{r:?}");
        assert!(r
            .assertions
            .iter()
            .any(|a| !a.ok && a.detail.contains("ERROR happened")));

        std::fs::write(&log, b"\xff\xfe bad\n").unwrap();
        let spec = Spec::from_yaml(&format!(
            "name: wl2\nspawn: [\"cat\"]\nsteps:\n  - watch_log: {{path: \"{}\", reject_re: \"x\"}}\n",
            log.display()
        ))
        .unwrap();
        let r = run_case(&spec, "xterm", 40, 10, &opts(dir.path())).await;
        assert!(
            r.error.as_deref().unwrap_or("").contains("not UTF-8"),
            "{r:?}"
        );
    }
}
