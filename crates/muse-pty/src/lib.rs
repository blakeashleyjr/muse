//! `muse-pty` — async wrapper over `portable-pty` (§7).
//!
//! `portable-pty` readers are blocking, so a dedicated thread bridges reads into
//! a tokio mpsc channel of `Bytes`.

use bytes::Bytes;
use muse_core::error::{Error, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub mod remote;

#[derive(Clone, Debug, Default)]
pub struct SpawnOpts {
    pub argv: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub cols: u16,
    pub rows: u16,
}

/// The exit status of the SUT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitStatus {
    pub code: u32,
    pub success: bool,
}

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    rx: mpsc::Receiver<Bytes>,
    pid: Option<u32>,
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl Pty {
    pub fn spawn(opts: SpawnOpts) -> Result<Pty> {
        if opts.argv.is_empty() {
            return Err(Error::BadArgument("argv is empty".into()));
        }
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(size(opts.cols, opts.rows))
            .map_err(|e| Error::SpawnFailed(e.to_string()))?;

        let mut cmd = CommandBuilder::new(&opts.argv[0]);
        cmd.args(&opts.argv[1..]);
        if let Some(cwd) = &opts.cwd {
            cmd.cwd(cwd);
        }
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::SpawnFailed(e.to_string()))?;
        let pid = child.process_id();

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::SpawnFailed(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| Error::SpawnFailed(e.to_string()))?;

        // Drop the slave so EOF propagates when the child exits.
        drop(pair.slave);

        let (tx, rx) = mpsc::channel::<Bytes>(256);
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.blocking_send(Bytes::copy_from_slice(&buf[..n])).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Pty {
            master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
            child,
            rx,
            pid,
        })
    }

    /// Read the next chunk of SUT output; `None` on EOF.
    pub async fn read(&mut self) -> Option<Bytes> {
        self.rx.recv().await
    }

    /// Direct access to the output receiver (for use in `select!` loops where
    /// the receiver must be a field disjoint from other awaited channels).
    pub fn reader(&mut self) -> &mut mpsc::Receiver<Bytes> {
        &mut self.rx
    }

    /// Try to read without awaiting (used to drain).
    pub fn try_read(&mut self) -> Option<Bytes> {
        self.rx.try_recv().ok()
    }

    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| Error::Internal("pty writer poisoned".into()))?;
        w.write_all(bytes)
            .map_err(|e| Error::TerminalCrashed(e.to_string()))?;
        w.flush()
            .map_err(|e| Error::TerminalCrashed(e.to_string()))?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .resize(size(cols, rows))
            .map_err(|e| Error::Internal(e.to_string()))
    }

    /// Send a signal to the SUT's whole process group (Unix). `portable-pty`
    /// runs the child under `setsid`, so its pid is the pgid and helpers it
    /// forked (pagers, language servers, watchers) are reached too.
    #[cfg(unix)]
    fn signal_group(&self, sig: libc::c_int) {
        if let Some(pid) = self.pid {
            // SAFETY: plain kill(2) on a pgid we own; errors (ESRCH) are ignored.
            unsafe {
                libc::kill(-(pid as libc::pid_t), sig);
            }
        }
    }

    /// Kill the direct child only. Prefer [`Pty::terminate`], which also
    /// reaches the process group and reaps the child.
    pub fn kill(&mut self) -> Result<()> {
        self.child
            .kill()
            .map_err(|e| Error::Internal(e.to_string()))
    }

    /// Terminate the SUT and everything in its process group: SIGTERM, wait up
    /// to `grace` for a clean exit, then SIGKILL — and always reap the child so
    /// no zombie outlives the session.
    pub fn terminate(&mut self, grace: std::time::Duration) -> ExitStatus {
        if let Some(st) = self.try_wait() {
            return st;
        }
        #[cfg(unix)]
        {
            self.signal_group(libc::SIGTERM);
            let t0 = std::time::Instant::now();
            while t0.elapsed() < grace {
                if let Some(st) = self.try_wait() {
                    return st;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            self.signal_group(libc::SIGKILL);
        }
        #[cfg(not(unix))]
        {
            let _ = grace;
            let _ = self.child.kill();
        }
        self.wait()
    }

    /// Wait for the child to exit.
    pub fn wait(&mut self) -> ExitStatus {
        match self.child.wait() {
            Ok(s) => ExitStatus {
                code: s.exit_code(),
                success: s.success(),
            },
            Err(_) => ExitStatus {
                code: 1,
                success: false,
            },
        }
    }

    /// Non-blocking exit check.
    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        match self.child.try_wait() {
            Ok(Some(s)) => Some(ExitStatus {
                code: s.exit_code(),
                success: s.success(),
            }),
            _ => None,
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn opts(argv: &[&str]) -> SpawnOpts {
        SpawnOpts {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            cols: 40,
            rows: 10,
            ..Default::default()
        }
    }

    async fn read_until(pty: &mut Pty, needle: &str, max: Duration) -> String {
        let mut acc = String::new();
        let deadline = tokio::time::Instant::now() + max;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, pty.read()).await {
                Ok(Some(b)) => {
                    acc.push_str(&String::from_utf8_lossy(&b));
                    if acc.contains(needle) {
                        break;
                    }
                }
                _ => break,
            }
        }
        acc
    }

    #[tokio::test]
    async fn empty_argv_errors() {
        assert!(Pty::spawn(opts(&[])).is_err());
    }

    #[tokio::test]
    async fn spawn_nonexistent_command_errors() {
        let r = Pty::spawn(opts(&["muse-no-such-binary-xyz-12345"]));
        assert!(matches!(r, Err(muse_core::error::Error::SpawnFailed(_))));
    }

    #[tokio::test]
    async fn cwd_is_respected() {
        let mut o = opts(&["sh", "-c", "pwd"]);
        o.cwd = Some(std::path::PathBuf::from("/tmp"));
        let mut pty = Pty::spawn(o).unwrap();
        let out = read_until(&mut pty, "/tmp", Duration::from_secs(5)).await;
        assert!(out.contains("/tmp"), "got: {out:?}");
    }

    #[tokio::test]
    async fn resize_after_exit_is_ok_or_err_no_panic() {
        let mut pty = Pty::spawn(opts(&["true"])).unwrap();
        let _ = pty.wait();
        // resizing a dead pty should not panic
        let _ = pty.resize(10, 10);
    }

    #[tokio::test]
    async fn spawn_echo() {
        let mut pty = Pty::spawn(opts(&["echo", "hello-pty"])).unwrap();
        let out = read_until(&mut pty, "hello-pty", Duration::from_secs(5)).await;
        assert!(out.contains("hello-pty"), "got: {out:?}");
        assert!(pty.pid().is_some());
    }

    #[tokio::test]
    async fn cat_echoes_input() {
        let mut pty = Pty::spawn(opts(&["cat"])).unwrap();
        pty.write(b"ping\n").unwrap();
        let out = read_until(&mut pty, "ping", Duration::from_secs(5)).await;
        assert!(out.contains("ping"), "got: {out:?}");
        pty.kill().unwrap();
    }

    #[tokio::test]
    async fn resize_ok() {
        let pty = Pty::spawn(opts(&["cat"])).unwrap();
        assert!(pty.resize(80, 24).is_ok());
    }

    #[tokio::test]
    async fn wait_reports_exit() {
        let mut pty = Pty::spawn(opts(&["true"])).unwrap();
        let st = pty.wait();
        assert!(st.success);
        assert_eq!(st.code, 0);
    }

    #[tokio::test]
    async fn false_exits_nonzero() {
        let mut pty = Pty::spawn(opts(&["false"])).unwrap();
        let st = pty.wait();
        assert!(!st.success);
    }

    #[tokio::test]
    async fn read_eof_after_exit() {
        let mut pty = Pty::spawn(opts(&["echo", "x"])).unwrap();
        // drain to EOF
        let mut saw_none = false;
        for _ in 0..100 {
            match tokio::time::timeout(Duration::from_secs(2), pty.read()).await {
                Ok(None) => {
                    saw_none = true;
                    break;
                }
                Ok(Some(_)) => continue,
                Err(_) => break,
            }
        }
        assert!(saw_none);
    }

    #[tokio::test]
    async fn env_passed_through() {
        let mut o = opts(&["sh", "-c", "echo V=$MUSE_TEST_VAR"]);
        o.env.insert("MUSE_TEST_VAR".into(), "xyz".into());
        let mut pty = Pty::spawn(o).unwrap();
        let out = read_until(&mut pty, "V=xyz", Duration::from_secs(5)).await;
        assert!(out.contains("V=xyz"), "got: {out:?}");
    }

    #[tokio::test]
    async fn try_wait_before_and_after_exit() {
        let mut pty = Pty::spawn(opts(&["sleep", "10"])).unwrap();
        assert!(pty.try_wait().is_none());
        pty.kill().unwrap();
        // poll until it reports exit
        let mut seen = None;
        for _ in 0..50 {
            if let Some(st) = pty.try_wait() {
                seen = Some(st);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(seen.is_some());
    }

    #[tokio::test]
    async fn try_read_drains() {
        let mut pty = Pty::spawn(opts(&["echo", "drainme"])).unwrap();
        // wait for output to arrive
        let _ = read_until(&mut pty, "drainme", Duration::from_secs(5)).await;
        // try_read may return remaining chunks or None; should not panic
        let _ = pty.try_read();
    }

    #[tokio::test]
    async fn reader_accessor_works() {
        let mut pty = Pty::spawn(opts(&["echo", "viareader"])).unwrap();
        let mut acc = String::new();
        for _ in 0..50 {
            match tokio::time::timeout(Duration::from_secs(2), pty.reader().recv()).await {
                Ok(Some(b)) => {
                    acc.push_str(&String::from_utf8_lossy(&b));
                    if acc.contains("viareader") {
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(acc.contains("viareader"));
    }

    #[tokio::test]
    async fn write_after_exit_errors_eventually() {
        let mut pty = Pty::spawn(opts(&["true"])).unwrap();
        let _ = pty.wait();
        // writing to a closed pty should error at some point (or succeed if
        // buffered); just ensure it does not panic.
        let _ = pty.write(b"data");
    }

    #[tokio::test]
    async fn read_until_times_out_when_absent() {
        // exercises the read helper's deadline/timeout arms against a quiet SUT
        let mut pty = Pty::spawn(opts(&["cat"])).unwrap();
        let out = read_until(&mut pty, "WILL-NEVER-APPEAR", Duration::from_millis(200)).await;
        assert!(!out.contains("WILL-NEVER-APPEAR"));
        pty.kill().unwrap();
    }

    #[tokio::test]
    async fn drop_while_producing_stops_reader() {
        // a never-ending producer; dropping the Pty closes the receiver, so the
        // reader thread's send fails and it exits cleanly (no leak, no panic).
        let mut pty = Pty::spawn(opts(&["sh", "-c", "while true; do echo x; done"])).unwrap();
        // consume a chunk to ensure the reader thread is actively sending
        let _ = tokio::time::timeout(Duration::from_secs(2), pty.read()).await;
        pty.kill().unwrap();
        drop(pty);
        // give the reader thread a moment to observe the closed channel
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn kill_terminates() {
        let mut pty = Pty::spawn(opts(&["sleep", "30"])).unwrap();
        pty.kill().unwrap();
        let st = pty.wait();
        assert!(!st.success);
    }
}
