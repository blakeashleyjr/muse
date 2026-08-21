//! `muse serve`: the session daemon. Owns a table of live terminals
//! ([`TerminalHandle`]s) keyed by session id and answers one NDJSON request
//! per connection on a unix socket. Started on demand by the first
//! `muse session open`; exits on its own once idle.

use super::client::{daemon_dir, ensure_dir};
use super::proto::{
    Input, Op, Request, Response, ScreenInfo, SessionInfo, WaitCond, PROTOCOL_VERSION,
};
use muse_core::config::SyncConfig;
use muse_core::error::{Error, Result};
use muse_core::snapshot::Snapshot;
use muse_engine::{assert, resolve_profile, spawn_terminal, TerminalHandle};
use muse_trace::TraceMeta;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;

struct Entry {
    handle: TerminalHandle,
    info: SessionInfo,
}

#[derive(Default)]
struct Table {
    next: u64,
    sessions: HashMap<String, Entry>,
    names: HashMap<String, String>,
}

struct State {
    table: Mutex<Table>,
    last_activity: Mutex<Instant>,
    dir: PathBuf,
    shutdown: Notify,
}

impl State {
    fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    fn lookup(&self, id: &str) -> Result<TerminalHandle> {
        let t = self.table.lock().unwrap();
        let key = t.names.get(id).cloned().unwrap_or_else(|| id.to_string());
        t.sessions
            .get(&key)
            .map(|e| e.handle.clone())
            .ok_or_else(|| Error::NotFound(format!("no session `{id}`")))
    }

    fn info(&self, id: &str) -> Result<SessionInfo> {
        let t = self.table.lock().unwrap();
        let key = t.names.get(id).cloned().unwrap_or_else(|| id.to_string());
        t.sessions
            .get(&key)
            .map(|e| e.info.clone())
            .ok_or_else(|| Error::NotFound(format!("no session `{id}`")))
    }

    fn remove(&self, id: &str) -> Option<Entry> {
        let mut t = self.table.lock().unwrap();
        let key = t.names.get(id).cloned().unwrap_or_else(|| id.to_string());
        let e = t.sessions.remove(&key)?;
        if let Some(n) = &e.info.name {
            t.names.remove(n);
        }
        Some(e)
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Hold an exclusive advisory lock on `<dir>/muse.lock` for the daemon's
/// lifetime so two daemons never race for one socket.
struct Lock(#[allow(dead_code)] std::fs::File);

fn take_lock(dir: &Path) -> Result<Option<Lock>> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join("muse.lock"))
        .map_err(|e| Error::Internal(format!("muse.lock: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: flock on an fd we own.
        let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Ok(None);
        }
    }
    Ok(Some(Lock(f)))
}

/// Run the daemon until idle for `idle` (with no sessions) or told to stop.
/// Returns `Ok(false)` immediately if another daemon already owns the dir.
pub async fn serve(sock: &Path, idle: Duration) -> Result<bool> {
    let dir = daemon_dir(sock);
    ensure_dir(&dir)?;
    let _lock = match take_lock(&dir)? {
        Some(l) => l,
        None => return Ok(false),
    };
    // Stale socket from a crashed daemon: we hold the lock, so nobody is
    // listening on it.
    let _ = std::fs::remove_file(sock);
    let listener = UnixListener::bind(sock)
        .map_err(|e| Error::Internal(format!("bind {}: {e}", sock.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600));
    }
    eprintln!(
        "muse serve: listening on {} (pid {}, idle {}s)",
        sock.display(),
        std::process::id(),
        idle.as_secs()
    );
    let state = Arc::new(State {
        table: Mutex::new(Table::default()),
        last_activity: Mutex::new(Instant::now()),
        dir: dir.clone(),
        shutdown: Notify::new(),
    });
    let mut ticker =
        tokio::time::interval(Duration::from_secs(1).min(idle.max(Duration::from_millis(50))));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let st = state.clone();
                        tokio::spawn(async move { handle_conn(stream, st).await });
                    }
                    Err(e) => eprintln!("muse serve: accept: {e}"),
                }
            }
            _ = state.shutdown.notified() => break,
            _ = ticker.tick() => {
                if reap_and_check_idle(&state, idle).await {
                    eprintln!("muse serve: idle, exiting");
                    break;
                }
            }
        }
    }
    close_all(&state).await;
    let _ = std::fs::remove_file(sock);
    Ok(true)
}

/// Close sessions whose SUT has been dead for longer than `idle`; report
/// whether the daemon itself should exit.
async fn reap_and_check_idle(state: &State, idle: Duration) -> bool {
    let ids: Vec<String> = state
        .table
        .lock()
        .unwrap()
        .sessions
        .keys()
        .cloned()
        .collect();
    for id in ids {
        if let Ok(h) = state.lookup(&id) {
            if let Ok(info) = h.info().await {
                if info.exit_code.is_some() {
                    let dead_for = state.last_activity.lock().unwrap().elapsed();
                    if dead_for > idle {
                        if let Some(e) = state.remove(&id) {
                            let _ = e.handle.shutdown().await;
                        }
                    }
                }
            }
        }
    }
    let empty = state.table.lock().unwrap().sessions.is_empty();
    empty && state.last_activity.lock().unwrap().elapsed() > idle
}

async fn close_all(state: &State) -> Vec<String> {
    let entries: Vec<(String, TerminalHandle)> = {
        let mut t = state.table.lock().unwrap();
        t.names.clear();
        t.sessions.drain().map(|(id, e)| (id, e.handle)).collect()
    };
    let mut closed = Vec::new();
    for (id, h) in entries {
        let _ = h.shutdown().await;
        closed.push(id);
    }
    closed.sort();
    closed
}

async fn handle_conn(stream: UnixStream, state: Arc<State>) {
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let resp = match serde_json::from_str::<Request>(line.trim_end()) {
        Ok(req) if req.v != PROTOCOL_VERSION => Response::error(format!(
            "protocol mismatch: client {} daemon {PROTOCOL_VERSION}",
            req.v
        )),
        Ok(req) => {
            state.touch();
            match dispatch(req.op, &state).await {
                Ok(r) => r,
                Err(e) => Response::error(e.to_string()),
            }
        }
        Err(e) => Response::error(format!("bad request: {e}")),
    };
    let mut out = serde_json::to_string(&resp).unwrap_or_else(|e| {
        serde_json::to_string(&Response::error(e.to_string())).unwrap_or_default()
    });
    out.push('\n');
    let _ = wr.write_all(out.as_bytes()).await;
    let _ = wr.shutdown().await;
}

fn render_text(screen: &muse_core::screen::Screen) -> String {
    muse_render::text::render_text(screen)
}

async fn dispatch(op: Op, state: &State) -> Result<Response> {
    match op {
        Op::Ping => Ok(Response::Pong {
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            pid: std::process::id(),
            sessions: state.table.lock().unwrap().sessions.len(),
        }),
        Op::Open {
            argv,
            profile,
            cols,
            rows,
            cwd,
            env,
            name,
            trace,
            quiet_window_ms,
            max_settle_ms,
        } => {
            open(
                state,
                argv,
                profile,
                cols,
                rows,
                cwd,
                env,
                name,
                trace,
                quiet_window_ms,
                max_settle_ms,
            )
            .await
        }
        Op::Send { id, input } => {
            let h = state.lookup(&id)?;
            match input {
                Input::Text { text } => h.write(text.into_bytes()).await?,
                Input::Bytes { hex } => h.write(decode_hex(&hex)?).await?,
                Input::Key { key } => h.key(key).await?,
                Input::Paste { text } => h.paste(text.into_bytes()).await?,
                Input::Mouse { ev } => h.mouse(ev).await?,
            }
            Ok(Response::Ack)
        }
        Op::Resize { id, cols, rows } => {
            let h = state.lookup(&id)?;
            h.resize(cols, rows).await?;
            {
                let mut t = state.table.lock().unwrap();
                let key = t.names.get(&id).cloned().unwrap_or_else(|| id.clone());
                if let Some(e) = t.sessions.get_mut(&key) {
                    e.info.cols = cols;
                    e.info.rows = rows;
                }
            }
            Ok(Response::Ack)
        }
        Op::Snap {
            id,
            kind,
            min_stable,
            timeout_ms,
            out,
        } => {
            let h = state.lookup(&id)?;
            let info = state.info(&id)?;
            let snap = h.snapshot(kind, min_stable, timeout_ms).await?;
            let (_, generation) = h.screen().await?;
            Ok(match snap {
                Snapshot::Text(t) => Response::Snap {
                    text: Some(t),
                    png: None,
                    width: None,
                    height: None,
                    generation,
                },
                Snapshot::Styled(s) => Response::Snap {
                    text: Some(s.to_canonical()),
                    png: None,
                    width: None,
                    height: None,
                    generation,
                },
                Snapshot::Pixel(p) => {
                    let path =
                        out.unwrap_or_else(|| info.dir.join(format!("snap-{generation}.png")));
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    std::fs::write(&path, &p.png)
                        .map_err(|e| Error::Internal(format!("write {}: {e}", path.display())))?;
                    Response::Snap {
                        text: None,
                        png: Some(path),
                        width: Some(p.width),
                        height: Some(p.height),
                        generation,
                    }
                }
            })
        }
        Op::Screen { id } => {
            let h = state.lookup(&id)?;
            let (screen, generation) = h.screen().await?;
            let grid = screen.active_grid();
            Ok(Response::Screen {
                screen: ScreenInfo {
                    cols: grid.cols(),
                    rows: grid.rows(),
                    cursor: screen.cursor,
                    title: screen.title.clone(),
                    modes: screen.modes.clone(),
                    alt_screen: screen.active == muse_core::screen::ScreenKind::Alt,
                    generation,
                    text: render_text(&screen),
                },
            })
        }
        Op::Wait {
            id,
            cond,
            timeout_ms,
        } => {
            let h = state.lookup(&id)?;
            let o = match cond {
                WaitCond::Visible { loc, multiline } => {
                    assert::to_be_visible(&h, loc, multiline, timeout_ms).await?
                }
                WaitCond::NotVisible { loc, multiline } => {
                    assert::to_not_be_visible(&h, loc, multiline, timeout_ms).await?
                }
                WaitCond::Text {
                    loc,
                    equals,
                    multiline,
                } => assert::to_have_text(&h, loc, &equals, multiline, timeout_ms).await?,
                WaitCond::Contains {
                    loc,
                    contains,
                    multiline,
                } => assert::to_contain_text(&h, loc, &contains, multiline, timeout_ms).await?,
                WaitCond::Count {
                    loc,
                    eq,
                    min,
                    max,
                    multiline,
                } => assert::to_have_count(&h, loc, eq, min, max, multiline, timeout_ms).await?,
                WaitCond::Exit => {
                    let code = h.wait_exit(timeout_ms).await?;
                    return Ok(Response::Wait {
                        ok: code.is_some(),
                        actual: code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "running".into()),
                        expected: "exited".into(),
                        detail: if code.is_some() {
                            String::new()
                        } else {
                            format!("still running after {timeout_ms}ms")
                        },
                        exit_code: code,
                    });
                }
            };
            let _ = h
                .record_assertion(
                    "wait",
                    o.ok,
                    format!("expected={} actual={} {}", o.expected, o.actual, o.detail),
                )
                .await;
            let exit_code = h.info().await.ok().and_then(|i| i.exit_code);
            Ok(Response::Wait {
                ok: o.ok,
                actual: o.actual,
                expected: o.expected,
                detail: o.detail,
                exit_code,
            })
        }
        Op::Logs { id } => {
            let h = state.lookup(&id)?;
            let info = state.info(&id)?;
            if !info.trace {
                return Err(Error::NotFound(
                    "session was opened with --no-trace; no output log".into(),
                ));
            }
            let dir = h.export_trace().await?;
            let cast = dir.join("output.cast");
            let text = cast_output_text(&cast)?;
            Ok(Response::Logs { text, cast })
        }
        Op::Trace { id, out } => {
            let h = state.lookup(&id)?;
            let dir = h.export_trace().await?;
            copy_dir(&dir, &out)?;
            Ok(Response::Trace { dir: out })
        }
        Op::List => {
            let handles: Vec<(String, TerminalHandle)> = state
                .table
                .lock()
                .unwrap()
                .sessions
                .iter()
                .map(|(k, e)| (k.clone(), e.handle.clone()))
                .collect();
            let mut sessions = Vec::new();
            for (id, h) in handles {
                let mut info = state.info(&id)?;
                if let Ok(ti) = h.info().await {
                    info.exit_code = ti.exit_code;
                    info.pid = ti.pid;
                }
                sessions.push(info);
            }
            sessions.sort_by(|a, b| a.opened_at.cmp(&b.opened_at).then(a.id.cmp(&b.id)));
            Ok(Response::List { sessions })
        }
        Op::Close { id, all } => {
            if all {
                let closed = close_all(state).await;
                return Ok(Response::Closed { closed });
            }
            let id = id.ok_or_else(|| Error::BadArgument("close: need an id or --all".into()))?;
            let e = state
                .remove(&id)
                .ok_or_else(|| Error::NotFound(format!("no session `{id}`")))?;
            let _ = e.handle.shutdown().await;
            Ok(Response::Closed {
                closed: vec![e.info.id],
            })
        }
        Op::Shutdown => {
            let closed = close_all(state).await;
            state.shutdown.notify_one();
            Ok(Response::Closed { closed })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn open(
    state: &State,
    argv: Vec<String>,
    profile: String,
    cols: u16,
    rows: u16,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    name: Option<String>,
    trace: bool,
    quiet_window_ms: Option<u64>,
    max_settle_ms: Option<u64>,
) -> Result<Response> {
    if argv.is_empty() {
        return Err(Error::BadArgument("open: empty argv".into()));
    }
    if let Some(n) = &name {
        if state.table.lock().unwrap().names.contains_key(n) {
            return Err(Error::BadArgument(format!("session name `{n}` is taken")));
        }
    }
    let prof = resolve_profile(&profile)?;
    let mut sync = SyncConfig::default();
    if let Some(q) = quiet_window_ms {
        sync.quiet_window_ms = q;
    }
    if let Some(m) = max_settle_ms {
        sync.max_settle_ms = m;
    }
    let env_map: HashMap<String, String> = env.iter().cloned().collect();
    let handle = spawn_terminal(prof, cols, rows, argv.clone(), env_map, cwd, sync)?;
    let id = {
        let mut t = state.table.lock().unwrap();
        t.next += 1;
        format!("s{}", t.next)
    };
    let dir = state.dir.join("sessions").join(&id);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Internal(format!("{}: {e}", dir.display())))?;
    if trace {
        handle
            .start_trace(
                dir.join("trace"),
                TraceMeta {
                    version: 1,
                    profile: profile.clone(),
                    cols,
                    rows,
                    env,
                    started_at: unix_now(),
                    sut_argv: argv.clone(),
                },
            )
            .await?;
    }
    let pid = handle.info().await.ok().and_then(|i| i.pid);
    let info = SessionInfo {
        id: id.clone(),
        name: name.clone(),
        argv,
        profile,
        cols,
        rows,
        pid,
        exit_code: None,
        opened_at: unix_now(),
        dir,
        trace,
    };
    {
        let mut t = state.table.lock().unwrap();
        if let Some(n) = &name {
            t.names.insert(n.clone(), id.clone());
        }
        t.sessions.insert(
            id,
            Entry {
                handle,
                info: info.clone(),
            },
        );
    }
    Ok(Response::Opened { session: info })
}

fn decode_hex(hex: &str) -> Result<Vec<u8>> {
    let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if !clean.len().is_multiple_of(2) {
        return Err(Error::BadArgument("odd-length hex".into()));
    }
    (0..clean.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&clean[i..i + 2], 16)
                .map_err(|e| Error::BadArgument(format!("bad hex: {e}")))
        })
        .collect()
}

/// Concatenate the `o` (output) payloads of an asciinema v2 cast.
pub fn cast_output_text(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| Error::Internal(format!("read {}: {e}", path.display())))?;
    let mut out = String::new();
    for line in raw.lines().skip(1) {
        if let Ok(serde_json::Value::Array(v)) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get(1).and_then(|c| c.as_str()) == Some("o") {
                if let Some(s) = v.get(2).and_then(|d| d.as_str()) {
                    out.push_str(s);
                }
            }
        }
    }
    Ok(out)
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(|e| Error::Internal(format!("{}: {e}", to.display())))?;
    for entry in
        std::fs::read_dir(from).map_err(|e| Error::Internal(format!("{}: {e}", from.display())))?
    {
        let entry = entry.map_err(|e| Error::Internal(e.to_string()))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)
                .map_err(|e| Error::Internal(format!("copy {}: {e}", src.display())))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::client::request;
    use super::*;
    use muse_core::locator::Locator;
    use muse_core::snapshot::SnapshotKind;

    fn text(p: &str) -> Locator {
        Locator::Text {
            pattern: p.into(),
            ignore_case: false,
            whole_line: false,
        }
    }

    async fn rq(sock: &Path, op: Op) -> Response {
        request(sock, &Request::new(op)).await.unwrap()
    }

    fn open_op(argv: &[&str], trace: bool) -> Op {
        Op::Open {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            profile: "xterm".into(),
            cols: 40,
            rows: 10,
            cwd: None,
            env: vec![],
            name: None,
            trace,
            quiet_window_ms: None,
            max_settle_ms: None,
        }
    }

    #[tokio::test]
    async fn full_session_lifecycle_in_process() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("muse.sock");
        let s2 = sock.clone();
        let server = tokio::spawn(async move { serve(&s2, Duration::from_millis(400)).await });
        for _ in 0..100 {
            if super::super::client::ping(&sock).await.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let Response::Pong { protocol, .. } = rq(&sock, Op::Ping).await else {
            panic!("no pong")
        };
        assert_eq!(protocol, PROTOCOL_VERSION);

        let Response::Opened { session } = rq(&sock, open_op(&["cat"], true)).await else {
            panic!("open")
        };
        let id = session.id.clone();
        assert!(session.pid.is_some());

        rq(
            &sock,
            Op::Send {
                id: id.clone(),
                input: Input::Text {
                    text: "hello-session\n".into(),
                },
            },
        )
        .await;
        let Response::Wait { ok, .. } = rq(
            &sock,
            Op::Wait {
                id: id.clone(),
                cond: WaitCond::Visible {
                    loc: text("hello-session"),
                    multiline: false,
                },
                timeout_ms: 3000,
            },
        )
        .await
        else {
            panic!("wait")
        };
        assert!(ok);

        let Response::Snap { text: t, .. } = rq(
            &sock,
            Op::Snap {
                id: id.clone(),
                kind: SnapshotKind::Text,
                min_stable: 1,
                timeout_ms: 2000,
                out: None,
            },
        )
        .await
        else {
            panic!("snap")
        };
        assert!(t.unwrap().contains("hello-session"));

        let png_path = dir.path().join("shot.png");
        let Response::Snap { png, width, .. } = rq(
            &sock,
            Op::Snap {
                id: id.clone(),
                kind: SnapshotKind::Pixel { scale: 1 },
                min_stable: 1,
                timeout_ms: 2000,
                out: Some(png_path.clone()),
            },
        )
        .await
        else {
            panic!("snap png")
        };
        assert_eq!(png.unwrap(), png_path);
        assert!(width.unwrap() > 0);
        assert!(png_path.exists());

        let Response::Screen { screen } = rq(&sock, Op::Screen { id: id.clone() }).await else {
            panic!("screen")
        };
        assert_eq!((screen.cols, screen.rows), (40, 10));
        assert!(screen.text.contains("hello-session"));

        rq(
            &sock,
            Op::Resize {
                id: id.clone(),
                cols: 60,
                rows: 12,
            },
        )
        .await;
        let Response::List { sessions } = rq(&sock, Op::List).await else {
            panic!("list")
        };
        assert_eq!(sessions.len(), 1);
        assert_eq!((sessions[0].cols, sessions[0].rows), (60, 12));

        let Response::Logs { text: logs, cast } = rq(&sock, Op::Logs { id: id.clone() }).await
        else {
            panic!("logs")
        };
        assert!(logs.contains("hello-session"), "{logs:?}");
        assert!(cast.exists());

        let out = dir.path().join("trace-copy");
        let Response::Trace { dir: tdir } = rq(
            &sock,
            Op::Trace {
                id: id.clone(),
                out: out.clone(),
            },
        )
        .await
        else {
            panic!("trace")
        };
        assert!(tdir.join("meta.json").exists());
        assert!(tdir.join("steps.jsonl").exists());

        let Response::Closed { closed } = rq(
            &sock,
            Op::Close {
                id: Some(id.clone()),
                all: false,
            },
        )
        .await
        else {
            panic!("close")
        };
        assert_eq!(closed, vec![id.clone()]);
        let Response::List { sessions } = rq(&sock, Op::List).await else {
            panic!("list2")
        };
        assert!(sessions.is_empty());
        assert!(matches!(
            rq(
                &sock,
                Op::Snap {
                    id,
                    kind: SnapshotKind::Text,
                    min_stable: 1,
                    timeout_ms: 10,
                    out: None
                }
            )
            .await,
            Response::Error { .. }
        ));

        // idle exit: no sessions + 400ms quiet
        let r = tokio::time::timeout(Duration::from_secs(5), server).await;
        assert!(matches!(r, Ok(Ok(Ok(true)))), "{r:?}");
        assert!(!sock.exists());
    }

    #[tokio::test]
    async fn names_exit_codes_and_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("muse.sock");
        let s2 = sock.clone();
        let server = tokio::spawn(async move { serve(&s2, Duration::from_secs(60)).await });
        for _ in 0..100 {
            if super::super::client::ping(&sock).await.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut op = open_op(&["sh", "-c", "echo bye; exit 3"], false);
        if let Op::Open { name, .. } = &mut op {
            *name = Some("short".into());
        }
        let Response::Opened { session } = rq(&sock, op.clone()).await else {
            panic!("open")
        };
        assert!(
            matches!(rq(&sock, op).await, Response::Error { .. }),
            "dup name"
        );
        // by name
        let Response::Wait { ok, exit_code, .. } = rq(
            &sock,
            Op::Wait {
                id: "short".into(),
                cond: WaitCond::Exit,
                timeout_ms: 3000,
            },
        )
        .await
        else {
            panic!("wait exit")
        };
        assert!(ok);
        assert_eq!(exit_code, Some(3));
        // still listed (with exit code) until closed; no-trace → logs errors
        let Response::List { sessions } = rq(&sock, Op::List).await else {
            panic!("list")
        };
        assert_eq!(sessions[0].exit_code, Some(3));
        assert!(matches!(
            rq(
                &sock,
                Op::Logs {
                    id: session.id.clone()
                }
            )
            .await,
            Response::Error { .. }
        ));
        assert!(matches!(
            rq(
                &sock,
                Op::Send {
                    id: session.id,
                    input: Input::Bytes { hex: "zz".into() }
                }
            )
            .await,
            Response::Error { .. }
        ));
        let Response::Closed { closed } = rq(&sock, Op::Shutdown).await else {
            panic!("shutdown")
        };
        assert_eq!(closed.len(), 1);
        let r = tokio::time::timeout(Duration::from_secs(5), server).await;
        assert!(matches!(r, Ok(Ok(Ok(true)))));
        // a second daemon on the same dir while one holds the lock → false
    }

    #[tokio::test]
    async fn second_daemon_yields_to_lock_holder() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("muse.sock");
        let s2 = sock.clone();
        let server = tokio::spawn(async move { serve(&s2, Duration::from_secs(60)).await });
        for _ in 0..100 {
            if super::super::client::ping(&sock).await.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let second = serve(&sock, Duration::from_secs(1)).await.unwrap();
        assert!(!second);
        rq(&sock, Op::Shutdown).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }

    #[test]
    fn hex_and_cast_helpers() {
        assert_eq!(decode_hex("1b 5b 41").unwrap(), b"\x1b[A");
        assert!(decode_hex("abc").is_err());
        assert!(decode_hex("zz").is_err());
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("o.cast");
        std::fs::write(
            &p,
            "{\"version\":2}\n[0.1,\"o\",\"ab\"]\n[0.2,\"i\",\"zz\"]\n[0.3,\"o\",\"c\"]\n",
        )
        .unwrap();
        assert_eq!(cast_output_text(&p).unwrap(), "abc");
    }
}
