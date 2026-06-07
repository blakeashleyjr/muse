//! Integration tests that exercise the compiled `muse` binary end-to-end,
//! covering `main.rs` (arg parse → dispatch → exit code).

use std::process::Command;

fn muse() -> Command {
    Command::new(env!("CARGO_BIN_EXE_muse"))
}

#[test]
fn profiles_command_runs() {
    let out = muse().arg("profiles").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("xterm"));
    assert!(stdout.contains("vt220"));
}

#[test]
fn doctor_command_runs() {
    let out = muse().arg("doctor").output().unwrap();
    assert!(out.status.success(), "doctor should pass self-test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("self-test: PASS"));
}

#[test]
fn exec_command_runs() {
    let out = muse()
        .args(["exec", "--", "echo", "binary-exec"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("binary-exec"));
}

#[test]
fn codegen_command_runs() {
    let out = muse().arg("codegen").output().unwrap();
    assert!(out.status.success());
}

#[test]
fn run_command_exit_code_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("fail.yaml");
    std::fs::write(
        &spec,
        "name: f\nspawn: [\"echo\", \"x\"]\nsteps:\n  - expect_visible: {text: \"NOPE\"}\n",
    )
    .unwrap();
    let out = muse()
        .args([
            "run",
            spec.to_str().unwrap(),
            "--profile",
            "xterm",
            "--size",
            "30x8",
            "--snapshots-dir",
            dir.path().join("s").to_str().unwrap(),
            "--deadline-ms",
            "300",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "failing suite must exit non-zero");
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAIL"));
}

#[test]
fn no_args_shows_error() {
    let out = muse().output().unwrap();
    // clap prints usage to stderr and exits non-zero when subcommand missing
    assert!(!out.status.success());
}
