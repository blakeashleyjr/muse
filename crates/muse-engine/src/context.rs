//! Session / Context management (§14). Each Context is an isolation unit:
//! one PTY + one emulator + one synchronizer + one trace.

use crate::terminal::TerminalHandle;
use muse_core::config::SyncConfig;
use muse_core::error::{Error, Result};
use muse_core::Profile;
use muse_emulator::{profile, VtEmulator};
use muse_pty::{Pty, SpawnOpts};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Build the environment for the SUT from a profile (§7).
pub fn build_env(profile: &Profile, extra: &HashMap<String, String>) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("TERM".to_string(), profile.caps.terminfo_name.clone());
    if profile.caps.color == muse_core::ColorDepth::TrueColor {
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
    }
    for (k, v) in &profile.env {
        env.insert(k.clone(), v.clone());
    }
    for (k, v) in extra {
        env.insert(k.clone(), v.clone());
    }
    env
}

/// An isolation unit. Created empty; `spawn` starts the SUT.
pub struct Context {
    pub id: u64,
    profile: Profile,
    cols: u16,
    rows: u16,
    sync_cfg: SyncConfig,
    handle: Option<TerminalHandle>,
}

impl Context {
    pub fn new(profile: Profile, cols: u16, rows: u16, sync_cfg: SyncConfig) -> Context {
        Context {
            id: next_id(),
            profile,
            cols,
            rows,
            sync_cfg,
            handle: None,
        }
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub fn dims(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Spawn the SUT and start the Terminal actor. Must be called within a
    /// tokio runtime.
    pub fn spawn(
        &mut self,
        argv: Vec<String>,
        env: HashMap<String, String>,
        cwd: Option<PathBuf>,
    ) -> Result<&TerminalHandle> {
        let full_env = build_env(&self.profile, &env);
        let pty = Pty::spawn(SpawnOpts {
            argv,
            env: full_env,
            cwd,
            cols: self.cols,
            rows: self.rows,
        })?;
        let emu = Box::new(VtEmulator::new(self.profile.clone(), self.cols, self.rows));
        let handle = TerminalHandle::spawn(pty, emu, self.sync_cfg.clone());
        self.handle = Some(handle);
        Ok(self.handle.as_ref().unwrap())
    }

    pub fn terminal(&self) -> Result<&TerminalHandle> {
        self.handle
            .as_ref()
            .ok_or_else(|| Error::NotFound("terminal not spawned".into()))
    }
}

/// A session groups contexts.
pub struct Session {
    pub id: u64,
    contexts: HashMap<u64, Context>,
}

impl Session {
    pub fn new() -> Session {
        Session {
            id: next_id(),
            contexts: HashMap::new(),
        }
    }

    pub fn new_context(
        &mut self,
        profile: Profile,
        cols: u16,
        rows: u16,
        sync_cfg: SyncConfig,
    ) -> u64 {
        let ctx = Context::new(profile, cols, rows, sync_cfg);
        let id = ctx.id;
        self.contexts.insert(id, ctx);
        id
    }

    pub fn context_mut(&mut self, id: u64) -> Result<&mut Context> {
        self.contexts
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("context {id}")))
    }

    pub fn close_context(&mut self, id: u64) -> bool {
        self.contexts.remove(&id).is_some()
    }

    pub fn context_count(&self) -> usize {
        self.contexts.len()
    }
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

/// Resolve a built-in profile by name.
pub fn resolve_profile(name: &str) -> Result<Profile> {
    profile::by_name(name).ok_or_else(|| Error::BadArgument(format!("unknown profile `{name}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_env_sets_term_and_colorterm() {
        let p = profile::xterm();
        let env = build_env(&p, &HashMap::new());
        assert_eq!(env.get("TERM").unwrap(), "xterm-256color");
        assert_eq!(env.get("COLORTERM").unwrap(), "truecolor");
    }

    #[test]
    fn build_env_no_colorterm_for_vt220() {
        let p = profile::vt220();
        let env = build_env(&p, &HashMap::new());
        assert!(!env.contains_key("COLORTERM"));
        assert_eq!(env.get("TERM").unwrap(), "vt220");
    }

    #[test]
    fn build_env_extra_overrides() {
        let p = profile::xterm();
        let mut extra = HashMap::new();
        extra.insert("TERM".to_string(), "custom".to_string());
        let env = build_env(&p, &extra);
        assert_eq!(env.get("TERM").unwrap(), "custom");
    }

    #[test]
    fn resolve_profile_ok_and_err() {
        assert!(resolve_profile("xterm").is_ok());
        assert!(resolve_profile("bogus").is_err());
    }

    #[test]
    fn context_unspawned_terminal_errors() {
        let ctx = Context::new(profile::xterm(), 80, 24, SyncConfig::default());
        assert!(ctx.terminal().is_err());
        assert_eq!(ctx.dims(), (80, 24));
        assert_eq!(ctx.profile().name, "xterm");
    }

    #[test]
    fn session_context_lifecycle() {
        let mut s = Session::new();
        let id = s.new_context(profile::xterm(), 80, 24, SyncConfig::default());
        assert_eq!(s.context_count(), 1);
        assert!(s.context_mut(id).is_ok());
        assert!(s.context_mut(99999).is_err());
        assert!(s.close_context(id));
        assert!(!s.close_context(id));
        assert_eq!(s.context_count(), 0);
    }

    #[test]
    fn ids_unique() {
        let a = Context::new(profile::xterm(), 80, 24, SyncConfig::default());
        let b = Context::new(profile::xterm(), 80, 24, SyncConfig::default());
        assert_ne!(a.id, b.id);
    }
}
