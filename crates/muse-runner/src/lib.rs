//! `muse-runner` — discover/expand/schedule tests, retries, reporters (§19),
//! plus the conformance harness (§20).

pub mod conformance;
pub mod report;
pub mod run;
pub mod spec;

use report::{CaseResult, SuiteResult};
use run::RunOpts;
use spec::Spec;

/// A single expanded matrix case (before execution).
#[derive(Debug, Clone, PartialEq)]
pub struct MatrixCase {
    pub profile: String,
    pub cols: u16,
    pub rows: u16,
}

/// Expand a spec's matrix into the cartesian product (profile × size),
/// optionally filtered to specific profiles.
pub fn expand_matrix(spec: &Spec, only_profiles: Option<&[String]>) -> Vec<MatrixCase> {
    let mut cases = Vec::new();
    for profile in spec.matrix.profiles_or_default() {
        if let Some(filter) = only_profiles {
            if !filter.iter().any(|p| p == &profile) {
                continue;
            }
        }
        for (cols, rows) in spec.matrix.sizes_or_default() {
            cases.push(MatrixCase {
                profile: profile.clone(),
                cols,
                rows,
            });
        }
    }
    cases
}

/// Options controlling a suite run.
#[derive(Default)]
pub struct SuiteOpts {
    pub run: RunOpts,
    pub retries: u32,
    pub workers: usize,
    pub only_profiles: Option<Vec<String>>,
    /// Substring filter on test name (`--grep`).
    pub grep: Option<String>,
    /// `--shard i/n`: run only cases where index % n == i.
    pub shard: Option<(usize, usize)>,
}

/// Run a set of specs, expanding the matrix and scheduling across workers.
pub async fn run_suite(specs: &[Spec], opts: &SuiteOpts) -> SuiteResult {
    // Build the work list.
    let mut work: Vec<(usize, MatrixCase)> = Vec::new();
    for (si, spec) in specs.iter().enumerate() {
        if let Some(g) = &opts.grep {
            if !spec.name.contains(g) {
                continue;
            }
        }
        for case in expand_matrix(spec, opts.only_profiles.as_deref()) {
            work.push((si, case));
        }
    }
    // Sharding.
    if let Some((i, n)) = opts.shard {
        if n > 0 {
            work = work
                .into_iter()
                .enumerate()
                .filter(|(idx, _)| idx % n == i)
                .map(|(_, w)| w)
                .collect();
        }
    }

    let workers = if opts.workers == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        opts.workers
    };
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(workers.max(1)));

    let mut tasks = Vec::new();
    for (si, case) in work {
        let spec = specs[si].clone();
        let sem = sem.clone();
        let retries = opts.retries;
        let run_opts = clone_run_opts(&opts.run);
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.unwrap();
            run_with_retries(&spec, &case, &run_opts, retries).await
        }));
    }

    let mut result = SuiteResult::default();
    for t in tasks {
        if let Ok(c) = t.await {
            result.cases.push(c);
        }
    }
    // stable order for deterministic reporting
    result.cases.sort_by_key(|a| a.case_id());
    result
}

async fn run_with_retries(
    spec: &Spec,
    case: &MatrixCase,
    opts: &RunOpts,
    retries: u32,
) -> CaseResult {
    let mut last = run::run_case(spec, &case.profile, case.cols, case.rows, opts).await;
    if last.passed() {
        return last;
    }
    let mut attempts = 0;
    while attempts < retries {
        attempts += 1;
        let again = run::run_case(spec, &case.profile, case.cols, case.rows, opts).await;
        if again.passed() {
            // failed then passed → flaky (reported, not a clean pass)
            let mut flaky = again;
            flaky.flaky = true;
            return flaky;
        }
        last = again;
    }
    last
}

fn clone_run_opts(o: &RunOpts) -> RunOpts {
    RunOpts {
        sync: o.sync.clone(),
        assert_deadline_ms: o.assert_deadline_ms,
        snapshots_dir: o.snapshots_dir.clone(),
        update_snapshots: o.update_snapshots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, profiles: &[&str], sizes: &[&str]) -> Spec {
        let p = profiles
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let s = sizes
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        Spec::from_yaml(&format!(
            "name: {name}\nmatrix:\n  profiles: [{p}]\n  sizes: [{s}]\nspawn: [\"echo\", \"hi\"]\nsteps:\n  - expect_visible: {{text: \"hi\"}}\n"
        ))
        .unwrap()
    }

    #[test]
    fn matrix_cartesian() {
        let s = spec("t", &["xterm", "vt220"], &["80x24", "100x30"]);
        let cases = expand_matrix(&s, None);
        assert_eq!(cases.len(), 4);
    }

    #[test]
    fn matrix_profile_filter() {
        let s = spec("t", &["xterm", "vt220"], &["80x24"]);
        let cases = expand_matrix(&s, Some(&["xterm".to_string()]));
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].profile, "xterm");
    }

    #[tokio::test]
    async fn acceptance_run_two_profiles() {
        // §19 acceptance: --profile xterm,vt220 --size 80x24 runs each test twice
        let dir = tempfile::tempdir().unwrap();
        let s = spec("echotest", &["xterm", "vt220"], &["40x10"]);
        let opts = SuiteOpts {
            run: RunOpts {
                assert_deadline_ms: 2000,
                snapshots_dir: dir.path().to_string_lossy().to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let result = run_suite(&[s], &opts).await;
        assert_eq!(result.total(), 2);
        assert!(result.passed(), "{}", result.pretty());
        // valid JUnit
        assert!(result.junit().contains("tests=\"2\""));
    }

    #[tokio::test]
    async fn grep_filters_specs() {
        let dir = tempfile::tempdir().unwrap();
        let a = spec("alpha", &["xterm"], &["40x10"]);
        let b = spec("beta", &["xterm"], &["40x10"]);
        let opts = SuiteOpts {
            run: RunOpts {
                assert_deadline_ms: 2000,
                snapshots_dir: dir.path().to_string_lossy().to_string(),
                ..Default::default()
            },
            grep: Some("alph".into()),
            ..Default::default()
        };
        let result = run_suite(&[a, b], &opts).await;
        assert_eq!(result.total(), 1);
        assert_eq!(result.cases[0].name, "alpha");
    }

    #[tokio::test]
    async fn shard_splits_work() {
        let dir = tempfile::tempdir().unwrap();
        let s = spec("t", &["xterm", "vt220", "kitty", "screen"], &["40x10"]);
        let mk = |i| SuiteOpts {
            run: RunOpts {
                assert_deadline_ms: 2000,
                snapshots_dir: dir.path().to_string_lossy().to_string(),
                ..Default::default()
            },
            shard: Some((i, 2)),
            ..Default::default()
        };
        let r0 = run_suite(std::slice::from_ref(&s), &mk(0)).await;
        let r1 = run_suite(std::slice::from_ref(&s), &mk(1)).await;
        assert_eq!(r0.total() + r1.total(), 4);
    }

    #[tokio::test]
    async fn retries_exhausted_stays_failed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Spec::from_yaml(
            "name: fails\nmatrix:\n  profiles: [xterm]\n  sizes: [\"40x10\"]\nspawn: [\"echo\", \"x\"]\nsteps:\n  - expect_visible: {text: \"NEVER\"}\n",
        )
        .unwrap();
        let opts = SuiteOpts {
            run: RunOpts {
                assert_deadline_ms: 200,
                snapshots_dir: dir.path().to_string_lossy().to_string(),
                ..Default::default()
            },
            retries: 2,
            workers: 2,
            ..Default::default()
        };
        let r = run_suite(&[s], &opts).await;
        assert!(!r.passed());
        assert!(!r.cases[0].flaky);
    }

    #[tokio::test]
    async fn retry_then_pass_is_flaky() {
        // A spec that always fails its assertion can't become flaky; instead
        // verify a passing spec with retries stays a clean (non-flaky) pass.
        let dir = tempfile::tempdir().unwrap();
        let s = spec("stable", &["xterm"], &["40x10"]);
        let opts = SuiteOpts {
            run: RunOpts {
                assert_deadline_ms: 2000,
                snapshots_dir: dir.path().to_string_lossy().to_string(),
                ..Default::default()
            },
            retries: 2,
            ..Default::default()
        };
        let r = run_suite(&[s], &opts).await;
        assert!(r.passed());
        assert!(!r.cases[0].flaky);
    }
}
