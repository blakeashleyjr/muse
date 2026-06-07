//! Configuration (§22): `muse.toml` + env + defaults with precedence.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    pub socket: String,
    pub protocol_version: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            socket: "/tmp/muse.sock".into(),
            protocol_version: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    pub quiet_window_ms: u64,
    pub max_settle_ms: u64,
    pub tick_ms: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        SyncConfig {
            quiet_window_ms: 50,
            max_settle_ms: 2000,
            tick_ms: 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultsConfig {
    pub profile: String,
    pub cols: u16,
    pub rows: u16,
    pub assert_deadline_ms: u64,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        DefaultsConfig {
            profile: "xterm".into(),
            cols: 80,
            rows: 24,
            assert_deadline_ms: 5000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotsConfig {
    pub dir: String,
    pub update: bool,
    pub pixel_scale: u8,
}

impl Default for SnapshotsConfig {
    fn default() -> Self {
        SnapshotsConfig {
            dir: "snapshots".into(),
            update: false,
            pixel_scale: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunnerConfig {
    pub workers: usize,
    pub retries: u32,
    pub reporter: String,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        RunnerConfig {
            workers: 0,
            retries: 0,
            reporter: "pretty".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizeEntry {
    pub re: String,
    pub replace: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub engine: EngineConfig,
    pub sync: SyncConfig,
    pub defaults: DefaultsConfig,
    pub snapshots: SnapshotsConfig,
    pub runner: RunnerConfig,
    pub normalize: Vec<NormalizeEntry>,
}

impl Config {
    /// Parse a config from a TOML string.
    pub fn from_toml(s: &str) -> Result<Config> {
        toml::from_str(s).map_err(|e| Error::BadArgument(format!("config parse: {e}")))
    }

    /// Apply `MUSE_*` env overrides. Precedence: env > toml (caller applies CLI
    /// on top of the returned value).
    pub fn apply_env<I, K, V>(&mut self, vars: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        for (k, v) in vars {
            let (k, v) = (k.as_ref(), v.as_ref());
            match k {
                "MUSE_SOCKET" => self.engine.socket = v.to_string(),
                "MUSE_PROFILE" => self.defaults.profile = v.to_string(),
                "MUSE_WORKERS" => {
                    if let Ok(n) = v.parse() {
                        self.runner.workers = n;
                    }
                }
                "MUSE_RETRIES" => {
                    if let Ok(n) = v.parse() {
                        self.runner.retries = n;
                    }
                }
                "MUSE_REPORTER" => self.runner.reporter = v.to_string(),
                "MUSE_QUIET_WINDOW_MS" => {
                    if let Ok(n) = v.parse() {
                        self.sync.quiet_window_ms = n;
                    }
                }
                "MUSE_SNAPSHOTS_DIR" => self.snapshots.dir = v.to_string(),
                "MUSE_UPDATE_SNAPSHOTS" => {
                    self.snapshots.update = matches!(v, "1" | "true" | "yes")
                }
                _ => {}
            }
        }
    }

    /// Validate ranges; called at startup.
    pub fn validate(&self) -> Result<()> {
        if self.defaults.cols == 0 || self.defaults.rows == 0 {
            return Err(Error::BadArgument("cols/rows must be > 0".into()));
        }
        if self.sync.quiet_window_ms > self.sync.max_settle_ms {
            return Err(Error::BadArgument(
                "quiet_window_ms must be <= max_settle_ms".into(),
            ));
        }
        for n in &self.normalize {
            regex::Regex::new(&n.re)
                .map_err(|e| Error::BadArgument(format!("normalize regex `{}`: {e}", n.re)))?;
        }
        Ok(())
    }

    /// The effective worker count (0 → number of CPUs).
    pub fn effective_workers(&self) -> usize {
        if self.runner.workers == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        } else {
            self.runner.workers
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_spec() {
        let c = Config::default();
        assert_eq!(c.sync.quiet_window_ms, 50);
        assert_eq!(c.sync.max_settle_ms, 2000);
        assert_eq!(c.defaults.cols, 80);
        assert_eq!(c.defaults.rows, 24);
        assert_eq!(c.defaults.assert_deadline_ms, 5000);
        assert_eq!(c.snapshots.dir, "snapshots");
    }

    #[test]
    fn parse_partial_toml() {
        let c = Config::from_toml(
            r#"
            [sync]
            quiet_window_ms = 100
            [defaults]
            profile = "vt220"
            "#,
        )
        .unwrap();
        assert_eq!(c.sync.quiet_window_ms, 100);
        assert_eq!(c.defaults.profile, "vt220");
        // unspecified keeps default
        assert_eq!(c.defaults.cols, 80);
    }

    #[test]
    fn parse_normalize_array() {
        let c = Config::from_toml(
            r#"
            [[normalize]]
            re = '\d+'
            replace = "N"
            "#,
        )
        .unwrap();
        assert_eq!(c.normalize.len(), 1);
        assert_eq!(c.normalize[0].replace, "N");
    }

    #[test]
    fn bad_toml_errors() {
        assert!(Config::from_toml("this is not = = toml").is_err());
    }

    #[test]
    fn env_overrides() {
        let mut c = Config::default();
        c.apply_env([
            ("MUSE_PROFILE", "kitty"),
            ("MUSE_WORKERS", "4"),
            ("MUSE_RETRIES", "2"),
            ("MUSE_REPORTER", "json"),
            ("MUSE_QUIET_WINDOW_MS", "75"),
            ("MUSE_SOCKET", "/x.sock"),
            ("MUSE_SNAPSHOTS_DIR", "snaps"),
            ("MUSE_UPDATE_SNAPSHOTS", "true"),
            ("UNRELATED", "x"),
        ]);
        assert_eq!(c.defaults.profile, "kitty");
        assert_eq!(c.runner.workers, 4);
        assert_eq!(c.runner.retries, 2);
        assert_eq!(c.runner.reporter, "json");
        assert_eq!(c.sync.quiet_window_ms, 75);
        assert_eq!(c.engine.socket, "/x.sock");
        assert_eq!(c.snapshots.dir, "snaps");
        assert!(c.snapshots.update);
    }

    #[test]
    fn env_ignores_bad_numbers() {
        let mut c = Config::default();
        c.apply_env([("MUSE_WORKERS", "notanumber")]);
        assert_eq!(c.runner.workers, 0);
    }

    #[test]
    fn validate_ok() {
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn validate_zero_dims() {
        let mut c = Config::default();
        c.defaults.cols = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_window_gt_settle() {
        let mut c = Config::default();
        c.sync.quiet_window_ms = 5000;
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_bad_normalize() {
        let mut c = Config::default();
        c.normalize.push(NormalizeEntry {
            re: "(".into(),
            replace: "x".into(),
        });
        assert!(c.validate().is_err());
    }

    #[test]
    fn effective_workers() {
        let mut c = Config::default();
        assert!(c.effective_workers() >= 1);
        c.runner.workers = 7;
        assert_eq!(c.effective_workers(), 7);
    }
}
