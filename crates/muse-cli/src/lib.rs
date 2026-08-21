//! `muse` CLI (§99): run / exec / trace / doctor / profiles / conformance.

pub mod conformance;
pub mod doctor;
pub mod exec;
pub mod trace_view;

use clap::{Parser, Subcommand};
use muse_core::config::SyncConfig;
use muse_emulator::profile;
use muse_runner::run::RunOpts;
use muse_runner::spec::Spec;
use muse_runner::SuiteOpts;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "muse",
    version,
    about = "Black-box e2e + visual-regression testing for terminal programs"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Run test spec files (matrix-expanded).
    Run(RunArgs),
    /// Spawn a program and dump a snapshot of one fidelity tier.
    Exec(ExecArgs),
    /// Inspect a recorded trace.
    Trace(TraceArgs),
    /// Print environment diagnostics + self-test.
    Doctor,
    /// List built-in emulation profiles.
    Profiles,
    /// Run emulator + protocol conformance corpora from a directory.
    Conformance(ConfArgs),
    /// Information about generating language SDKs.
    Codegen,
}

#[derive(clap::Args, Debug)]
pub struct RunArgs {
    /// Spec files (YAML).
    pub specs: Vec<PathBuf>,
    /// Override profiles (comma-separated), e.g. xterm,vt220.
    #[arg(long)]
    pub profile: Option<String>,
    /// Override sizes (comma-separated), e.g. 80x24,100x30.
    #[arg(long)]
    pub size: Option<String>,
    #[arg(long)]
    pub update_snapshots: bool,
    #[arg(long)]
    pub grep: Option<String>,
    /// Shard as i/n.
    #[arg(long)]
    pub shard: Option<String>,
    #[arg(long, default_value_t = 0)]
    pub retries: u32,
    #[arg(long, default_value_t = 0)]
    pub workers: usize,
    #[arg(long, default_value = "pretty")]
    pub reporter: String,
    #[arg(long, default_value = "snapshots")]
    pub snapshots_dir: String,
    #[arg(long, default_value_t = 5000)]
    pub deadline_ms: u64,
    /// CI mode: a snapshot with no committed baseline fails instead of
    /// silently creating one.
    #[arg(long)]
    pub ci: bool,
    /// Accept a run that selected zero cases.
    #[arg(long)]
    pub allow_empty: bool,
    /// Per-case wall-clock cap in ms (0 = none).
    #[arg(long, default_value_t = 120_000)]
    pub case_timeout_ms: u64,
    /// Where failure artifacts (final screen, diffs, trace) are written.
    /// `none` disables them.
    #[arg(long, default_value = "test-results")]
    pub artifacts: String,
    /// Trace recording: on | retain-on-failure | off.
    #[arg(long, default_value = "retain-on-failure")]
    pub trace: String,
}

#[derive(clap::Args, Debug)]
pub struct ExecArgs {
    /// Command and arguments (after `--`).
    #[arg(required = true, num_args = 1.., last = true)]
    pub argv: Vec<String>,
    #[arg(long, default_value = "xterm")]
    pub profile: String,
    #[arg(long, default_value = "80x24")]
    pub size: String,
    #[arg(long, default_value = "text")]
    pub kind: String,
    #[arg(long, default_value_t = 1)]
    pub scale: u8,
    /// Output file for pixel PNGs.
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[arg(long, default_value_t = 3000)]
    pub deadline_ms: u64,
}

#[derive(clap::Args, Debug)]
pub struct TraceArgs {
    pub dir: PathBuf,
    /// Print a specific frame's text.
    #[arg(long)]
    pub frame: Option<usize>,
}

#[derive(clap::Args, Debug)]
pub struct ConfArgs {
    pub dir: PathBuf,
    #[arg(long, default_value = "snapshots")]
    pub snapshots_dir: String,
}

/// Outcome of a command: text to print and process success.
pub struct Outcome {
    pub stdout: String,
    pub success: bool,
}

impl Outcome {
    fn ok(stdout: impl Into<String>) -> Outcome {
        Outcome {
            stdout: stdout.into(),
            success: true,
        }
    }
    fn fail(stdout: impl Into<String>) -> Outcome {
        Outcome {
            stdout: stdout.into(),
            success: false,
        }
    }
}

pub fn profiles_table() -> String {
    let mut s = String::from(
        "NAME     TERM                COLOR       KEYBOARD         MOUSE PASTE SYNC\n",
    );
    for name in profile::all_names() {
        let p = profile::by_name(name).unwrap();
        s.push_str(&format!(
            "{:<8} {:<19} {:<11?} {:<16?} {:<5} {:<5} {}\n",
            p.name,
            p.caps.terminfo_name,
            p.caps.color,
            p.caps.keyboard,
            !p.caps.mouse.is_empty(),
            p.caps.supports_bracketed_paste,
            p.caps.supports_sync_output,
        ));
    }
    s
}

pub fn codegen_info() -> String {
    "SDK base clients are generated from proto/muse/v1/muse.proto via buf:\n  \
     buf generate   # Go, TS, Python, C++ bases into sdks/*\n\
     Rust client/server are generated in-build via tonic-build.\n\
     Hand-written sugar layers live alongside the generated bases (§16, §18).\n\
     The embedded (in-process) engine used by `muse run`/`exec` needs no codegen.\n"
        .to_string()
}

async fn cmd_run(args: &RunArgs) -> Outcome {
    if args.specs.is_empty() {
        return Outcome::fail("no spec files given\n");
    }
    let mut specs = Vec::new();
    for path in &args.specs {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => return Outcome::fail(format!("read {}: {e}\n", path.display())),
        };
        match Spec::from_yaml(&text) {
            Ok(mut spec) => {
                if let Some(p) = &args.profile {
                    spec.matrix.profiles = p.split(',').map(|s| s.trim().to_string()).collect();
                }
                if let Some(sz) = &args.size {
                    spec.matrix.sizes = sz.split(',').map(|s| s.trim().to_string()).collect();
                }
                specs.push(spec);
            }
            Err(e) => return Outcome::fail(format!("parse {}: {e}\n", path.display())),
        }
    }
    let shard = args.shard.as_ref().and_then(|s| {
        let (i, n) = s.split_once('/')?;
        Some((i.parse().ok()?, n.parse().ok()?))
    });
    let trace = match muse_runner::run::TraceMode::parse(&args.trace) {
        Some(t) => t,
        None => {
            return Outcome::fail(format!(
                "bad --trace {:?}: expected on | retain-on-failure | off\n",
                args.trace
            ))
        }
    };
    let artifacts_dir = if args.artifacts == "none" {
        None
    } else {
        Some(PathBuf::from(&args.artifacts))
    };
    let opts = SuiteOpts {
        run: RunOpts {
            sync: SyncConfig::default(),
            assert_deadline_ms: args.deadline_ms,
            snapshots_dir: args.snapshots_dir.clone(),
            update_snapshots: args.update_snapshots,
            forbid_create: args.ci || std::env::var_os("MUSE_CI").is_some(),
            artifacts_dir,
            trace,
        },
        retries: args.retries,
        workers: args.workers,
        only_profiles: None,
        grep: args.grep.clone(),
        shard,
        allow_empty: args.allow_empty,
        case_timeout_ms: args.case_timeout_ms,
    };
    let result = muse_runner::run_suite(&specs, &opts).await;
    let report = match args.reporter.as_str() {
        "junit" => result.junit(),
        "json" => result.json(),
        _ => result.pretty(),
    };
    Outcome {
        stdout: report,
        success: result.passed(),
    }
}

async fn cmd_exec(args: &ExecArgs) -> Outcome {
    let (cols, rows) = match muse_runner::spec::parse_size(&args.size) {
        Some(v) => v,
        None => return Outcome::fail(format!("bad size {}\n", args.size)),
    };
    let opts = exec::ExecOpts {
        profile: args.profile.clone(),
        cols,
        rows,
        kind: exec::parse_kind(&args.kind, args.scale),
        deadline_ms: args.deadline_ms,
    };
    match exec::exec(args.argv.clone(), &opts).await {
        Ok(snap) => {
            let (text, png) = exec::present(&snap);
            if let (Some(png), Some(out)) = (png.as_ref(), args.out.as_ref()) {
                if let Err(e) = std::fs::write(out, png) {
                    return Outcome::fail(format!("write {}: {e}\n", out.display()));
                }
                Outcome::ok(format!("{text}\nwrote {}\n", out.display()))
            } else {
                Outcome::ok(format!("{text}\n"))
            }
        }
        Err(e) => Outcome::fail(format!("exec failed: {e}\n")),
    }
}

async fn cmd_trace(args: &TraceArgs) -> Outcome {
    match trace_view::load(&args.dir) {
        Ok(t) => {
            if let Some(idx) = args.frame {
                match trace_view::frame_text(&t, idx) {
                    Ok(text) => Outcome::ok(format!("{text}\n")),
                    Err(e) => Outcome::fail(format!("{e}\n")),
                }
            } else {
                Outcome::ok(trace_view::summary(&t))
            }
        }
        Err(e) => Outcome::fail(format!("load trace: {e}\n")),
    }
}

/// Dispatch a parsed CLI to an outcome.
pub async fn dispatch(cli: Cli) -> Outcome {
    match cli.cmd {
        Cmd::Run(args) => cmd_run(&args).await,
        Cmd::Exec(args) => cmd_exec(&args).await,
        Cmd::Trace(args) => cmd_trace(&args).await,
        Cmd::Doctor => {
            let r = doctor::run().await;
            Outcome {
                success: r.self_test_ok,
                stdout: r.render(),
            }
        }
        Cmd::Profiles => Outcome::ok(profiles_table()),
        Cmd::Conformance(args) => {
            let s = conformance::run_dir(&args.dir, &args.snapshots_dir).await;
            Outcome {
                success: s.ok(),
                stdout: s.render(),
            }
        }
        Cmd::Codegen => Outcome::ok(codegen_info()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_table_lists_all() {
        let t = profiles_table();
        for name in profile::all_names() {
            assert!(t.contains(name), "missing {name}");
        }
    }

    #[test]
    fn codegen_info_mentions_buf() {
        assert!(codegen_info().contains("buf"));
    }

    #[test]
    fn cli_parses_run() {
        let cli =
            Cli::try_parse_from(["muse", "run", "a.yaml", "--profile", "xterm,vt220"]).unwrap();
        match cli.cmd {
            Cmd::Run(a) => {
                assert_eq!(a.specs.len(), 1);
                assert_eq!(a.profile.as_deref(), Some("xterm,vt220"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn cli_parses_exec() {
        let cli =
            Cli::try_parse_from(["muse", "exec", "--kind", "pixel", "--", "echo", "hi"]).unwrap();
        match cli.cmd {
            Cmd::Exec(a) => {
                assert_eq!(a.argv, vec!["echo", "hi"]);
                assert_eq!(a.kind, "pixel");
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn dispatch_profiles() {
        let cli = Cli::try_parse_from(["muse", "profiles"]).unwrap();
        let o = dispatch(cli).await;
        assert!(o.success);
        assert!(o.stdout.contains("xterm"));
    }

    #[tokio::test]
    async fn dispatch_doctor() {
        let cli = Cli::try_parse_from(["muse", "doctor"]).unwrap();
        let o = dispatch(cli).await;
        assert!(o.success, "{}", o.stdout);
        assert!(o.stdout.contains("self-test: PASS"));
    }

    #[tokio::test]
    async fn dispatch_exec_text() {
        let cli = Cli::try_parse_from(["muse", "exec", "--", "echo", "cli-exec"]).unwrap();
        let o = dispatch(cli).await;
        assert!(o.success);
        assert!(o.stdout.contains("cli-exec"));
    }

    #[tokio::test]
    async fn dispatch_run_spec() {
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("t.yaml");
        std::fs::write(
            &spec_path,
            "name: t\nspawn: [\"echo\", \"clihi\"]\nsteps:\n  - expect_visible: {text: \"clihi\"}\n",
        )
        .unwrap();
        let snaps = dir.path().join("snaps");
        let cli = Cli::try_parse_from([
            "muse",
            "run",
            spec_path.to_str().unwrap(),
            "--profile",
            "xterm",
            "--size",
            "40x10",
            "--snapshots-dir",
            snaps.to_str().unwrap(),
            "--deadline-ms",
            "2000",
        ])
        .unwrap();
        let o = dispatch(cli).await;
        assert!(o.success, "{}", o.stdout);
        assert!(o.stdout.contains("1 passed"));
    }

    #[tokio::test]
    async fn dispatch_run_no_specs_fails() {
        let cli = Cli::try_parse_from(["muse", "run"]).unwrap();
        let o = dispatch(cli).await;
        assert!(!o.success);
    }

    #[tokio::test]
    async fn dispatch_run_junit_reporter_and_shard() {
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("t.yaml");
        std::fs::write(
            &spec_path,
            "name: t\nspawn: [\"echo\", \"j\"]\nsteps:\n  - expect_visible: {text: \"j\"}\n",
        )
        .unwrap();
        let snaps = dir.path().join("snaps");
        let cli = Cli::try_parse_from([
            "muse",
            "run",
            spec_path.to_str().unwrap(),
            "--profile",
            "xterm",
            "--size",
            "40x10",
            "--reporter",
            "junit",
            "--shard",
            "0/1",
            "--retries",
            "1",
            "--workers",
            "2",
            "--snapshots-dir",
            snaps.to_str().unwrap(),
            "--deadline-ms",
            "2000",
        ])
        .unwrap();
        let o = dispatch(cli).await;
        assert!(o.success, "{}", o.stdout);
        assert!(o.stdout.contains("<testsuites"));
    }

    #[tokio::test]
    async fn dispatch_run_json_reporter() {
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("t.yaml");
        std::fs::write(
            &spec_path,
            "name: t\nspawn: [\"echo\", \"j\"]\nsteps:\n  - expect_visible: {text: \"j\"}\n",
        )
        .unwrap();
        let snaps = dir.path().join("snaps");
        let cli = Cli::try_parse_from([
            "muse",
            "run",
            spec_path.to_str().unwrap(),
            "--reporter",
            "json",
            "--profile",
            "xterm",
            "--size",
            "40x10",
            "--snapshots-dir",
            snaps.to_str().unwrap(),
            "--deadline-ms",
            "2000",
        ])
        .unwrap();
        let o = dispatch(cli).await;
        assert!(o.stdout.contains("\"cases\""));
    }

    #[tokio::test]
    async fn dispatch_run_bad_spec_yaml_fails() {
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("bad.yaml");
        std::fs::write(&spec_path, "name: x\n  : : :\n").unwrap();
        let cli = Cli::try_parse_from(["muse", "run", spec_path.to_str().unwrap()]).unwrap();
        let o = dispatch(cli).await;
        assert!(!o.success);
        assert!(o.stdout.contains("parse"));
    }

    #[tokio::test]
    async fn dispatch_exec_pixel_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.png");
        let cli = Cli::try_parse_from([
            "muse",
            "exec",
            "--kind",
            "pixel",
            "--out",
            out.to_str().unwrap(),
            "--",
            "echo",
            "px",
        ])
        .unwrap();
        let o = dispatch(cli).await;
        assert!(o.success, "{}", o.stdout);
        assert!(out.exists());
        assert!(o.stdout.contains("wrote"));
    }

    #[tokio::test]
    async fn dispatch_exec_styled() {
        let cli =
            Cli::try_parse_from(["muse", "exec", "--kind", "styled", "--", "echo", "sx"]).unwrap();
        let o = dispatch(cli).await;
        assert!(o.success);
    }

    #[tokio::test]
    async fn shipped_corpus_is_green() {
        // §20 acceptance: the checked-in emulator + protocol corpora all pass.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let snaps = tempfile::tempdir().unwrap();
        for sub in ["conformance/emulator", "conformance/protocol"] {
            let dir = root.join(sub);
            let sum = conformance::run_dir(&dir, &snaps.path().to_string_lossy()).await;
            assert!(sum.passed > 0, "{sub}: no cases ran");
            assert!(sum.ok(), "{sub} corpus failed:\n{}", sum.render());
        }
    }

    #[tokio::test]
    async fn dispatch_conformance() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("sgr.yaml"),
            "name: sgr\nfeed: \"\\e[31mHI\"\nexpect:\n  lines:\n    - text: \"HI\"\n",
        )
        .unwrap();
        let snaps = dir.path().join("snaps");
        let cli = Cli::try_parse_from([
            "muse",
            "conformance",
            dir.path().to_str().unwrap(),
            "--snapshots-dir",
            snaps.to_str().unwrap(),
        ])
        .unwrap();
        let o = dispatch(cli).await;
        assert!(o.success, "{}", o.stdout);
        assert!(o.stdout.contains("passed"));
    }

    #[tokio::test]
    async fn dispatch_trace_frame() {
        // build a trace then view a frame
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tr");
        let mut rec = muse_trace::Recorder::new(
            &path,
            muse_trace::TraceMeta {
                version: 1,
                profile: "xterm".into(),
                cols: 5,
                rows: 2,
                env: vec![("TERM".into(), "xterm".into())],
                started_at: 0,
                sut_argv: vec![],
            },
        );
        let mut screen = muse_core::screen::Screen::new(2, 5);
        screen
            .primary
            .set(0, 0, muse_core::cell::Cell::glyph("Z", Default::default()));
        rec.on_frame(0.0, 1, screen);
        rec.flush().unwrap();
        let cli =
            Cli::try_parse_from(["muse", "trace", path.to_str().unwrap(), "--frame", "0"]).unwrap();
        let o = dispatch(cli).await;
        assert!(o.success);
        assert!(o.stdout.contains("Z"));
        // summary mode
        let cli2 = Cli::try_parse_from(["muse", "trace", path.to_str().unwrap()]).unwrap();
        assert!(dispatch(cli2).await.stdout.contains("frames=1"));
    }

    #[tokio::test]
    async fn dispatch_run_missing_file() {
        let cli = Cli::try_parse_from(["muse", "run", "/no/such/spec.yaml"]).unwrap();
        let o = dispatch(cli).await;
        assert!(!o.success);
        assert!(o.stdout.contains("read"));
    }

    #[tokio::test]
    async fn dispatch_codegen() {
        let cli = Cli::try_parse_from(["muse", "codegen"]).unwrap();
        let o = dispatch(cli).await;
        assert!(o.success);
    }

    #[tokio::test]
    async fn dispatch_trace_missing() {
        let cli = Cli::try_parse_from(["muse", "trace", "/no/trace"]).unwrap();
        let o = dispatch(cli).await;
        assert!(!o.success);
    }

    #[tokio::test]
    async fn dispatch_exec_bad_size() {
        let cli =
            Cli::try_parse_from(["muse", "exec", "--size", "bad", "--", "echo", "x"]).unwrap();
        let o = dispatch(cli).await;
        assert!(!o.success);
    }
}
