//! Client side of the session protocol: find (or start) the daemon and make
//! one request.

use super::proto::{Op, Request, Response, PROTOCOL_VERSION};
use muse_core::error::{Error, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Resolve the socket path: explicit flag > `MUSE_SOCKET` >
/// `$XDG_RUNTIME_DIR/muse/muse.sock` > `/tmp/muse-<uid>/muse.sock`.
pub fn socket_path(flag: Option<&Path>) -> PathBuf {
    if let Some(p) = flag {
        return p.to_path_buf();
    }
    if let Some(p) = std::env::var_os("MUSE_SOCKET") {
        return PathBuf::from(p);
    }
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(rt).join("muse").join("muse.sock");
    }
    #[cfg(unix)]
    let uid = {
        // SAFETY: getuid has no preconditions.
        unsafe { libc::getuid() }
    };
    #[cfg(not(unix))]
    let uid = 0;
    std::env::temp_dir()
        .join(format!("muse-{uid}"))
        .join("muse.sock")
}

/// The directory the daemon keeps its lock, log, and per-session files in.
pub fn daemon_dir(sock: &Path) -> PathBuf {
    sock.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn ensure_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| Error::Internal(format!("{}: {e}", dir.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// One request → one response over a fresh connection.
pub async fn request(sock: &Path, req: &Request) -> Result<Response> {
    let stream = UnixStream::connect(sock)
        .await
        .map_err(|e| Error::Internal(format!("connect {}: {e}", sock.display())))?;
    let (rd, mut wr) = stream.into_split();
    let mut line = serde_json::to_string(req).map_err(|e| Error::Internal(e.to_string()))?;
    line.push('\n');
    wr.write_all(line.as_bytes())
        .await
        .map_err(|e| Error::Internal(format!("write: {e}")))?;
    let mut reader = BufReader::new(rd);
    let mut resp = String::new();
    let n = reader
        .read_line(&mut resp)
        .await
        .map_err(|e| Error::Internal(format!("read: {e}")))?;
    if n == 0 {
        return Err(Error::Internal("daemon closed the connection".into()));
    }
    serde_json::from_str(resp.trim_end())
        .map_err(|e| Error::Internal(format!("bad response {resp:?}: {e}")))
}

/// Is a compatible daemon listening?
pub async fn ping(sock: &Path) -> Option<Response> {
    match request(sock, &Request::new(Op::Ping)).await {
        Ok(r @ Response::Pong { .. }) => Some(r),
        _ => None,
    }
}

/// Connect to the daemon, starting one (detached) if none is listening.
pub async fn connect_or_spawn(sock: &Path, idle_ms: u64) -> Result<()> {
    if let Some(Response::Pong { protocol, .. }) = ping(sock).await {
        if protocol != PROTOCOL_VERSION {
            return Err(Error::Internal(format!(
                "daemon speaks protocol {protocol}, this client {PROTOCOL_VERSION}; run `muse serve --stop`"
            )));
        }
        return Ok(());
    }
    let dir = daemon_dir(sock);
    ensure_dir(&dir)?;
    let exe = std::env::current_exe().map_err(|e| Error::Internal(format!("current_exe: {e}")))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("daemon.log"))
        .map_err(|e| Error::Internal(format!("daemon.log: {e}")))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("serve")
        .arg("--socket")
        .arg(sock)
        .arg("--idle-ms")
        .arg(idle_ms.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group: outlives the shell that ran `muse session open`.
        cmd.process_group(0);
    }
    cmd.spawn()
        .map_err(|e| Error::Internal(format!("spawn daemon: {e}")))?;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(5) {
        if ping(sock).await.is_some() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(Error::Internal(format!(
        "daemon did not come up on {} (see {})",
        sock.display(),
        dir.join("daemon.log").display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_resolution_order() {
        let flag = socket_path(Some(Path::new("/x/s.sock")));
        assert_eq!(flag, PathBuf::from("/x/s.sock"));
        let p = socket_path(None);
        assert!(p.ends_with("muse.sock"), "{p:?}");
        assert_eq!(
            daemon_dir(Path::new("/a/b/muse.sock")),
            PathBuf::from("/a/b")
        );
    }

    #[tokio::test]
    async fn request_without_daemon_errors() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("none.sock");
        assert!(request(&sock, &Request::new(Op::Ping)).await.is_err());
        assert!(ping(&sock).await.is_none());
    }
}
