//! Remote PTY transport (§7, P2). A trait abstraction over local/SSH transports.

use muse_core::error::Result;

/// A transport that can spawn a remote command and shuttle bytes. P2; only the
/// local path is wired up in v1, but the trait is defined so the engine can be
/// transport-agnostic.
pub trait Transport: Send {
    fn name(&self) -> &str;
    fn write(&self, bytes: &[u8]) -> Result<()>;
    fn resize(&self, cols: u16, rows: u16) -> Result<()>;
}

/// A host allowlist for remote PTYs (§24: explicit allowlist required).
#[derive(Clone, Debug, Default)]
pub struct HostAllowlist {
    hosts: Vec<String>,
}

impl HostAllowlist {
    pub fn new(hosts: Vec<String>) -> Self {
        HostAllowlist { hosts }
    }

    pub fn allows(&self, host: &str) -> bool {
        self.hosts.iter().any(|h| h == host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_checks() {
        let a = HostAllowlist::new(vec!["a.example".into()]);
        assert!(a.allows("a.example"));
        assert!(!a.allows("b.example"));
        assert!(!HostAllowlist::default().allows("anything"));
    }
}
