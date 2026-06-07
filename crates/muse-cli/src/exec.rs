//! `muse exec` — spawn a program and dump a snapshot of one tier.

use muse_core::config::SyncConfig;
use muse_core::error::{Error, Result};
use muse_core::snapshot::{Snapshot, SnapshotKind};
use muse_engine::{resolve_profile, spawn_terminal};
use std::collections::HashMap;

pub struct ExecOpts {
    pub profile: String,
    pub cols: u16,
    pub rows: u16,
    pub kind: SnapshotKind,
    pub deadline_ms: u64,
}

/// Spawn `argv`, settle, and return the snapshot.
pub async fn exec(argv: Vec<String>, opts: &ExecOpts) -> Result<Snapshot> {
    if argv.is_empty() {
        return Err(Error::BadArgument("no command given".into()));
    }
    let profile = resolve_profile(&opts.profile)?;
    let handle = spawn_terminal(
        profile,
        opts.cols,
        opts.rows,
        argv,
        HashMap::new(),
        None,
        SyncConfig::default(),
    )?;
    let snap = handle.snapshot(opts.kind, 1, opts.deadline_ms).await;
    handle.shutdown().await.ok();
    snap
}

/// Render a snapshot for stdout. Pixel snapshots return the PNG bytes via the
/// second tuple element (to be written to a file).
pub fn present(snap: &Snapshot) -> (String, Option<Vec<u8>>) {
    match snap {
        Snapshot::Text(t) => (t.clone(), None),
        Snapshot::Styled(s) => (s.to_canonical(), None),
        Snapshot::Pixel(p) => (
            format!("PNG {}x{} ({} bytes)", p.width, p.height, p.png.len()),
            Some(p.png.clone()),
        ),
    }
}

pub fn parse_kind(kind: &str, scale: u8) -> SnapshotKind {
    match kind {
        "styled" => SnapshotKind::Styled,
        "pixel" => SnapshotKind::Pixel { scale },
        _ => SnapshotKind::Text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(kind: SnapshotKind) -> ExecOpts {
        ExecOpts {
            profile: "xterm".into(),
            cols: 40,
            rows: 10,
            kind,
            deadline_ms: 2000,
        }
    }

    #[tokio::test]
    async fn exec_text() {
        let snap = exec(
            vec!["echo".into(), "exec-test".into()],
            &opts(SnapshotKind::Text),
        )
        .await
        .unwrap();
        let (text, png) = present(&snap);
        assert!(text.contains("exec-test"));
        assert!(png.is_none());
    }

    #[tokio::test]
    async fn exec_pixel_returns_png() {
        let snap = exec(
            vec!["echo".into(), "p".into()],
            &opts(SnapshotKind::Pixel { scale: 1 }),
        )
        .await
        .unwrap();
        let (_, png) = present(&snap);
        assert!(!png.unwrap().is_empty());
    }

    #[tokio::test]
    async fn exec_empty_argv_errs() {
        assert!(exec(vec![], &opts(SnapshotKind::Text)).await.is_err());
    }

    #[tokio::test]
    async fn exec_bad_profile_errs() {
        let mut o = opts(SnapshotKind::Text);
        o.profile = "nope".into();
        assert!(exec(vec!["echo".into()], &o).await.is_err());
    }

    #[test]
    fn parse_kind_variants() {
        assert_eq!(parse_kind("text", 1), SnapshotKind::Text);
        assert_eq!(parse_kind("styled", 1), SnapshotKind::Styled);
        assert_eq!(parse_kind("pixel", 2), SnapshotKind::Pixel { scale: 2 });
    }
}
