//! `muse-engine` — the in-process (embedded) engine: Terminal actor, session /
//! context management, synchronizer, and web-first assertions (§8, §14, §18).

pub mod assert;
pub mod context;
pub mod sync;
pub mod terminal;

use muse_core::config::Config;
use muse_core::error::{Error, Result};
use muse_core::Profile;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub use assert::AssertOutcome;
pub use context::{build_env, resolve_profile, Context, Session};
pub use sync::{SyncState, Synchronizer};
pub use terminal::{FrameEvent, TermCmd, Terminal, TerminalHandle, TerminalInfo};

/// Top-level manager holding sessions (§14: SessionManager → Session → Context).
pub struct Engine {
    config: Config,
    sessions: HashMap<u64, Session>,
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

impl Engine {
    pub fn new(config: Config) -> Engine {
        Engine {
            config,
            sessions: HashMap::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn create_session(&mut self) -> u64 {
        let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        self.sessions.insert(id, Session::new());
        id
    }

    pub fn session_mut(&mut self, id: u64) -> Result<&mut Session> {
        self.sessions
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("session {id}")))
    }

    pub fn close_session(&mut self, id: u64) -> bool {
        self.sessions.remove(&id).is_some()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

/// Ergonomic one-shot spawn used by the CLI/SDK: create a context for the named
/// profile and start the SUT, returning a handle. Must run within a tokio
/// runtime.
pub fn spawn_terminal(
    profile: Profile,
    cols: u16,
    rows: u16,
    argv: Vec<String>,
    env: HashMap<String, String>,
    cwd: Option<PathBuf>,
    sync_cfg: muse_core::config::SyncConfig,
) -> Result<TerminalHandle> {
    let mut ctx = Context::new(profile, cols, rows, sync_cfg);
    ctx.spawn(argv, env, cwd)?;
    Ok(ctx.terminal()?.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::config::SyncConfig;
    use muse_core::locator::Locator;
    use muse_core::snapshot::{Snapshot, SnapshotKind};
    use muse_emulator::profile;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn handle(argv_parts: &[&str]) -> Result<TerminalHandle> {
        spawn_terminal(
            profile::xterm(),
            40,
            10,
            argv(argv_parts),
            HashMap::new(),
            None,
            SyncConfig::default(),
        )
    }

    #[tokio::test]
    async fn key_press_echoes() {
        use muse_core::input::{Key, KeyEvent};
        let h = handle(&["cat"]).unwrap();
        h.key(KeyEvent::new(Key::Char('z'))).await.unwrap();
        h.key(KeyEvent::new(Key::Enter)).await.unwrap();
        let out = assert::to_contain_text(&h, Locator::Regex { re: "z".into() }, "z", false, 2000)
            .await
            .unwrap();
        assert!(out.ok, "{out:?}");
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn paste_echoes() {
        let h = handle(&["cat"]).unwrap();
        h.paste(b"pasted-text\n".to_vec()).await.unwrap();
        let out = assert::to_contain_text(
            &h,
            Locator::Regex {
                re: "pasted".into(),
            },
            "pasted",
            false,
            2000,
        )
        .await
        .unwrap();
        assert!(out.ok, "{out:?}");
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn mouse_while_disabled_is_noop() {
        use muse_core::input::{Mods, MouseAction, MouseButton, MouseEvent};
        let h = handle(&["cat"]).unwrap();
        h.mouse(MouseEvent {
            button: MouseButton::Left,
            action: MouseAction::Press,
            row: 0,
            col: 0,
            mods: Mods::empty(),
        })
        .await
        .unwrap();
        // still alive
        h.write(&b"after\n"[..]).await.unwrap();
        let out = assert::to_contain_text(
            &h,
            Locator::Regex { re: "after".into() },
            "after",
            false,
            2000,
        )
        .await
        .unwrap();
        assert!(out.ok);
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn mouse_enabled_writes_bytes() {
        use muse_core::input::{Mods, MouseAction, MouseButton, MouseEvent};
        // SUT enables SGR mouse tracking then becomes `cat` (long-lived) so the
        // mode is negotiated before we send the event.
        let h = handle(&["sh", "-c", "printf '\\033[?1006h\\033[?1000h'; exec cat"]).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        h.mouse(MouseEvent {
            button: MouseButton::Left,
            action: MouseAction::Press,
            row: 1,
            col: 2,
            mods: Mods::empty(),
        })
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn snapshot_times_out_against_live_program() {
        // cat never exits, so requiring 5 identical stable frames forces the
        // check_deadlines snapshot path (not the EOF finalize path).
        let h = handle(&["cat"]).unwrap();
        let snap = h.snapshot(SnapshotKind::Text, 5, 400).await.unwrap();
        assert!(matches!(snap, Snapshot::Text(_)));
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn snapshot_waiter_fires_on_frame() {
        // request a snapshot immediately (before the first stable frame) so it
        // registers a waiter that is resolved by fire_waiters on the next frame.
        let h = handle(&["sh", "-c", "echo waiterframe; exec cat"]).unwrap();
        let snap = h.snapshot(SnapshotKind::Text, 1, 3000).await.unwrap();
        match snap {
            Snapshot::Text(t) => assert!(t.contains("waiterframe"), "{t:?}"),
            _ => panic!(),
        }
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn da_reply_and_ready_marker() {
        // SUT emits a DA query (engine writes the reply back) and a muse:ready
        // marker (synchronizer short-circuits), then stays alive.
        let h = handle(&[
            "sh",
            "-c",
            "printf '\\033[c\\033]5379;muse:ready\\007READY-NOW'; exec cat",
        ])
        .unwrap();
        let v = assert::to_be_visible(
            &h,
            Locator::Text {
                pattern: "READY-NOW".into(),
                ignore_case: false,
                whole_line: false,
            },
            false,
            2000,
        )
        .await
        .unwrap();
        assert!(v.ok, "{v:?}");
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn identical_frames_increment_stable_run() {
        // write content, settle, then resize to the same dims → a second stable
        // frame with an identical screen (exercises the prev_stable == screen arm).
        let h = handle(&["cat"]).unwrap();
        h.write(&b"persist\n"[..]).await.unwrap();
        let _ = assert::to_contain_text(
            &h,
            Locator::Regex {
                re: "persist".into(),
            },
            "persist",
            false,
            2000,
        )
        .await
        .unwrap();
        h.resize(40, 10).await.unwrap(); // same dims → identical screen
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn trace_records_inputs_and_frames() {
        // recorder active while driving input → covers the on_input/on_frame arms.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tr2");
        let h = handle(&["cat"]).unwrap();
        h.start_trace(
            path.clone(),
            muse_trace::TraceMeta {
                version: 1,
                profile: "xterm".into(),
                cols: 40,
                rows: 10,
                env: vec![("TERM".into(), "xterm-256color".into())],
                started_at: 0,
                sut_argv: vec!["cat".into()],
            },
        )
        .await
        .unwrap();
        // Subscribed after start_trace, so any frame seen here is one the
        // recorder has already taken (`emit_stable` records before it
        // publishes). Awaiting one makes "a frame was traced" a fact rather
        // than a race against the quiet window.
        let mut frames = h.subscribe();
        h.write(&b"traced-write\n"[..]).await.unwrap();
        h.key(muse_core::input::KeyEvent::new(
            muse_core::input::Key::Char('k'),
        ))
        .await
        .unwrap();
        h.paste(&b"traced-paste\n"[..]).await.unwrap();
        let _ = assert::to_contain_text(
            &h,
            Locator::Regex {
                re: "traced-write".into(),
            },
            "traced-write",
            false,
            2000,
        )
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), frames.recv())
            .await
            .expect("no frame within 5s")
            .expect("frame channel closed");
        let out = h.export_trace().await.unwrap();
        let t = muse_trace::Trace::load(&out).unwrap();
        assert!(!t.frames.is_empty());
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn export_trace_without_active_trace_errors() {
        let h = handle(&["echo", "x"]).unwrap();
        assert!(h.export_trace().await.is_err());
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn steps_without_recorder_are_noops() {
        let h = handle(&["echo", "x"]).unwrap();
        h.begin_step("nostep").await.unwrap();
        h.end_step().await.unwrap();
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn drop_handle_triggers_eof_finalize() {
        // dropping all handles closes the cmd channel → actor finalizes
        let h = handle(&["sleep", "10"]).unwrap();
        drop(h);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    /// Regression: output that arrives after a settled frame, with no input
    /// from us in between, must be visible to a retrying assertion.
    #[tokio::test]
    async fn spontaneous_output_after_settle_is_seen() {
        let h = handle(&["sh", "-c", "echo first; sleep 0.4; echo LATE; exec cat"]).unwrap();
        let m = h
            .resolve(
                Locator::Text {
                    pattern: "first".into(),
                    ignore_case: false,
                    whole_line: false,
                },
                false,
                3000,
            )
            .await
            .unwrap();
        assert!(!m.is_empty());
        // no write/key here — the SUT repaints on its own
        let t0 = std::time::Instant::now();
        let m = h
            .resolve(
                Locator::Text {
                    pattern: "LATE".into(),
                    ignore_case: false,
                    whole_line: false,
                },
                false,
                5000,
            )
            .await
            .unwrap();
        assert!(!m.is_empty(), "late output never produced a frame");
        // must resolve from a fresh stable frame (~0.4s + quiet window), not
        // fall through to the best-effort deadline
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(2500),
            "resolved only at the deadline: {:?}",
            t0.elapsed()
        );
        h.shutdown().await.unwrap();
    }

    /// Regression: a program that takes a moment to paint must not be
    /// snapshotted as an empty screen.
    #[tokio::test]
    async fn late_first_output_not_settled_empty() {
        let h = handle(&["sh", "-c", "sleep 0.3; echo late; exec cat"]).unwrap();
        let snap = h.snapshot(SnapshotKind::Text, 1, 3000).await.unwrap();
        let text = match snap {
            Snapshot::Text(t) => t,
            _ => panic!("text"),
        };
        assert!(text.contains("late"), "blank snapshot: {text:?}");
        h.shutdown().await.unwrap();
    }

    /// Regression: shutdown stops the SUT's helpers too (process group), and
    /// reaps the child.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_kills_process_group() {
        let h = handle(&["sh", "-c", "sleep 100 & echo PID=$!; wait"]).unwrap();
        let m = h
            .resolve(
                Locator::Regex {
                    re: "PID=[0-9]+".into(),
                },
                false,
                3000,
            )
            .await
            .unwrap();
        let pid: i32 = m[0].text.trim_start_matches("PID=").trim().parse().unwrap();
        let info = h.info().await.unwrap();
        assert!(info.pid.is_some());
        assert!(info.exit_code.is_none());
        h.shutdown().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // SAFETY: signal 0 only probes existence
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "background helper {pid} outlived shutdown");
    }

    #[tokio::test]
    async fn screen_and_info_commands() {
        let h = handle(&["sh", "-c", "echo hello; exec cat"]).unwrap();
        h.resolve(
            Locator::Text {
                pattern: "hello".into(),
                ignore_case: false,
                whole_line: false,
            },
            false,
            3000,
        )
        .await
        .unwrap();
        let (screen, generation) = h.screen().await.unwrap();
        assert!(generation >= 1);
        assert_eq!(screen.active_grid().cols(), 40);
        let info = h.info().await.unwrap();
        assert!(!info.eof);
        h.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn engine_session_lifecycle() {
        let mut e = Engine::new(Config::default());
        assert_eq!(e.config().defaults.cols, 80);
        let sid = e.create_session();
        assert_eq!(e.session_count(), 1);
        let s = e.session_mut(sid).unwrap();
        let _cid = s.new_context(profile::xterm(), 80, 24, SyncConfig::default());
        assert!(e.session_mut(99999).is_err());
        assert!(e.close_session(sid));
        assert_eq!(e.session_count(), 0);
    }

    #[tokio::test]
    async fn acceptance_spawn_echo_resolve() {
        // §15 acceptance: NewContext -> Spawn(echo hi) -> ResolveLocator(Text hi)
        let h = handle(&["echo", "hi"]).unwrap();
        let m = h
            .resolve(
                Locator::Text {
                    pattern: "hi".into(),
                    ignore_case: false,
                    whole_line: false,
                },
                false,
                2000,
            )
            .await
            .unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text, "hi");
    }

    #[tokio::test]
    async fn cat_echo_roundtrip() {
        let h = handle(&["cat"]).unwrap();
        h.write(&b"ping\n"[..]).await.unwrap();
        let out = assert::to_contain_text(
            &h,
            Locator::Regex { re: "ping".into() },
            "ping",
            false,
            3000,
        )
        .await
        .unwrap();
        assert!(out.ok, "{out:?}");
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn snapshot_text() {
        let h = handle(&["echo", "snapshot-me"]).unwrap();
        let snap = h.snapshot(SnapshotKind::Text, 1, 2000).await.unwrap();
        match snap {
            Snapshot::Text(t) => assert!(t.contains("snapshot-me"), "{t:?}"),
            _ => panic!("expected text"),
        }
    }

    #[tokio::test]
    async fn snapshot_pixel_deterministic() {
        let h = handle(&["echo", "pix"]).unwrap();
        let a = h
            .snapshot(SnapshotKind::Pixel { scale: 1 }, 1, 2000)
            .await
            .unwrap();
        let b = h
            .snapshot(SnapshotKind::Pixel { scale: 1 }, 1, 2000)
            .await
            .unwrap();
        match (a, b) {
            (Snapshot::Pixel(pa), Snapshot::Pixel(pb)) => assert_eq!(pa.png, pb.png),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn assert_visible_and_text() {
        let h = handle(&["echo", "READY"]).unwrap();
        let v = assert::to_be_visible(
            &h,
            Locator::Text {
                pattern: "READY".into(),
                ignore_case: false,
                whole_line: false,
            },
            false,
            2000,
        )
        .await
        .unwrap();
        assert!(v.ok);
        let t = assert::to_have_text(&h, Locator::Line { row: 0 }, "READY", false, 2000)
            .await
            .unwrap();
        assert!(t.ok, "{t:?}");
    }

    #[tokio::test]
    async fn assert_not_visible_times_out() {
        let h = handle(&["echo", "x"]).unwrap();
        let v = assert::to_be_visible(
            &h,
            Locator::Text {
                pattern: "NOPE".into(),
                ignore_case: false,
                whole_line: false,
            },
            false,
            300,
        )
        .await
        .unwrap();
        assert!(!v.ok);
    }

    #[tokio::test]
    async fn resolve_unknown_returns_empty_at_deadline() {
        let h = handle(&["echo", "x"]).unwrap();
        let m = h
            .resolve(
                Locator::Text {
                    pattern: "ZZZ".into(),
                    ignore_case: false,
                    whole_line: false,
                },
                false,
                300,
            )
            .await
            .unwrap();
        assert!(m.is_empty());
    }

    #[tokio::test]
    async fn many_concurrent_contexts() {
        // §14 acceptance (scaled): many concurrent contexts each spawning cat.
        let mut tasks = Vec::new();
        for i in 0..40 {
            tasks.push(tokio::spawn(async move {
                let h = spawn_terminal(
                    profile::xterm(),
                    20,
                    5,
                    argv(&["cat"]),
                    HashMap::new(),
                    None,
                    SyncConfig::default(),
                )
                .unwrap();
                let msg = format!("echo{i}\n");
                h.write(msg.clone().into_bytes()).await.unwrap();
                let out = assert::to_contain_text(
                    &h,
                    Locator::Regex {
                        re: format!("echo{i}"),
                    },
                    &format!("echo{i}"),
                    false,
                    3000,
                )
                .await
                .unwrap();
                h.shutdown().await.ok();
                out.ok
            }));
        }
        for t in tasks {
            assert!(t.await.unwrap());
        }
    }

    #[tokio::test]
    async fn resize_reflows() {
        let h = handle(&["cat"]).unwrap();
        h.resize(80, 24).await.unwrap();
        // no panic / channel alive
        h.write(&b"hello\n"[..]).await.unwrap();
        let v = assert::to_contain_text(
            &h,
            Locator::Regex { re: "hello".into() },
            "hello",
            false,
            2000,
        )
        .await
        .unwrap();
        assert!(v.ok);
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn trace_records_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tr");
        let h = handle(&["echo", "traced"]).unwrap();
        h.start_trace(
            path.clone(),
            muse_trace::TraceMeta {
                version: 1,
                profile: "xterm".into(),
                cols: 40,
                rows: 10,
                env: vec![("TERM".into(), "xterm-256color".into())],
                started_at: 0,
                sut_argv: argv(&["echo", "traced"]),
            },
        )
        .await
        .unwrap();
        h.begin_step("s1").await.unwrap();
        // give the SUT a moment to emit and settle
        let _ = h
            .resolve(
                Locator::Text {
                    pattern: "traced".into(),
                    ignore_case: false,
                    whole_line: false,
                },
                false,
                2000,
            )
            .await
            .unwrap();
        h.end_step().await.unwrap();
        let out = h.export_trace().await.unwrap();
        let t = muse_trace::Trace::load(&out).unwrap();
        assert_eq!(t.meta.profile, "xterm");
        assert!(!t.steps.is_empty());
    }

    #[tokio::test]
    async fn set_profile_changes_caps() {
        let h = handle(&["cat"]).unwrap();
        h.set_profile(profile::vt220()).await.unwrap();
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn subscribe_receives_frames() {
        let h = handle(&["echo", "frame"]).unwrap();
        let mut rx = h.subscribe();
        // trigger resolve to ensure a frame is produced
        let _ = h
            .resolve(
                Locator::Text {
                    pattern: "frame".into(),
                    ignore_case: false,
                    whole_line: false,
                },
                false,
                2000,
            )
            .await
            .unwrap();
        // a frame event should be available
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
        assert!(got.is_ok());
    }
}
