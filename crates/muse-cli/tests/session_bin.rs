//! End-to-end: the `muse session` CLI against a daemon it auto-starts, through
//! the real binary, on an isolated socket. This is the agent workflow:
//! open → wait → send → snap → logs → close → serve --stop.

use std::path::Path;
use std::process::Command;

fn muse(sock: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_muse"));
    c.env("MUSE_SOCKET", sock);
    c
}

fn run(sock: &Path, args: &[&str]) -> (i32, String) {
    let out = muse(sock).args(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn agent_loop_through_the_binary() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("muse.sock");

    // open auto-spawns the daemon and prints an id
    let (code, id) = run(
        &sock,
        &[
            "session",
            "open",
            "--size",
            "50x8",
            "--name",
            "app",
            "--",
            "sh",
            "-c",
            "printf 'name? '; read n; echo \"hi $n\"; sleep 0.3; echo LATE; exec cat",
        ],
    );
    assert_eq!(code, 0, "{id}");
    let id = id.trim().to_string();
    assert!(id.starts_with('s'), "{id}");

    let (code, out) = run(&sock, &["session", "wait", "app", "--visible", "name?"]);
    assert_eq!(code, 0, "{out}");

    let (code, _) = run(
        &sock,
        &["session", "send", &id, "--text", "ada", "--key", "enter"],
    );
    assert_eq!(code, 0);

    // output that arrives with no input in between must be awaitable
    let (code, out) = run(
        &sock,
        &[
            "session",
            "wait",
            "app",
            "--visible",
            "LATE",
            "--timeout-ms",
            "4000",
        ],
    );
    assert_eq!(code, 0, "{out}");

    let (code, out) = run(&sock, &["session", "snap", "app"]);
    assert_eq!(code, 0);
    assert!(out.contains("hi ada") && out.contains("LATE"), "{out}");

    let png = dir.path().join("shot.png");
    let (code, out) = run(
        &sock,
        &[
            "session",
            "snap",
            "app",
            "--kind",
            "pixel",
            "--out",
            png.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{out}");
    assert!(png.exists());
    assert!(std::fs::read(&png).unwrap().starts_with(b"\x89PNG"));

    let (code, out) = run(&sock, &["session", "--json", "screen", "app"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["screen"]["cols"], 50);

    let (code, out) = run(&sock, &["session", "logs", "app"]);
    assert_eq!(code, 0);
    assert!(out.contains("name? ada"), "{out:?}");

    // a wait that fails is exit 1 (not 2), with the reason
    let (code, out) = run(
        &sock,
        &[
            "session",
            "wait",
            "app",
            "--visible",
            "NOPE",
            "--timeout-ms",
            "200",
        ],
    );
    assert_eq!(code, 1, "{out}");
    assert!(out.starts_with("FAIL:"), "{out}");

    // unknown session is a transport/usage error: exit 2
    let (code, _) = run(&sock, &["session", "snap", "nosuch"]);
    assert_eq!(code, 2);

    let (code, out) = run(&sock, &["session", "--json", "list"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["sessions"].as_array().unwrap().len(), 1);
    let daemon_pid = {
        let (_, pong) = run(&sock, &["session", "list"]);
        assert!(pong.contains("app"));
        std::fs::read_to_string(dir.path().join("daemon.log")).unwrap()
    };
    assert!(daemon_pid.contains("listening"));

    let (code, out) = run(&sock, &["session", "close", "app"]);
    assert_eq!(code, 0, "{out}");
    let (_, out) = run(&sock, &["session", "list"]);
    assert!(out.contains("no sessions"), "{out}");

    let (code, out) = run(&sock, &["serve", "--stop"]);
    assert_eq!(code, 0, "{out}");
    // socket gone, daemon gone
    for _ in 0..50 {
        if !sock.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!sock.exists());
    let (code, out) = run(&sock, &["session", "list"]);
    assert_eq!(code, 0);
    assert!(out.contains("no daemon"), "{out}");
}

#[test]
fn close_kills_the_program_group() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("muse.sock");
    let (code, _) = run(
        &sock,
        &[
            "session",
            "open",
            "--name",
            "bg",
            "--",
            "sh",
            "-c",
            "sleep 300 & echo PID=$!; wait",
        ],
    );
    assert_eq!(code, 0);
    let (code, out) = run(&sock, &["session", "wait", "bg", "--regex", "PID=[0-9]+"]);
    assert_eq!(code, 0, "{out}");
    let (_, screen) = run(&sock, &["session", "snap", "bg"]);
    let pid: i32 = screen
        .lines()
        .find_map(|l| l.strip_prefix("PID="))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let (code, _) = run(&sock, &["session", "close", "bg"]);
    assert_eq!(code, 0);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let alive = Path::new(&format!("/proc/{pid}")).exists();
    assert!(!alive, "background sleep {pid} survived close");
    run(&sock, &["serve", "--stop"]);
}
