//! `muse doctor` — environment self-test (§23).

use muse_core::config::SyncConfig;
use muse_core::locator::Locator;
use muse_emulator::profile;
use muse_engine::{assert, spawn_terminal};
use std::collections::HashMap;

pub struct DoctorReport {
    pub muse_version: String,
    pub font_fingerprint: u64,
    pub font_glyphs: usize,
    pub shells: Vec<(String, bool)>,
    pub profiles: Vec<String>,
    pub self_test_ok: bool,
    pub self_test_detail: String,
}

impl DoctorReport {
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("muse {}\n", self.muse_version));
        s.push_str(&format!(
            "font: fingerprint={:#018x} glyphs={}\n",
            self.font_fingerprint, self.font_glyphs
        ));
        s.push_str("shells:\n");
        for (name, ok) in &self.shells {
            s.push_str(&format!("  {} {}\n", if *ok { "✓" } else { "✗" }, name));
        }
        s.push_str(&format!("profiles: {}\n", self.profiles.join(", ")));
        s.push_str(&format!(
            "self-test: {} {}\n",
            if self.self_test_ok { "PASS" } else { "FAIL" },
            self.self_test_detail
        ));
        s
    }
}

fn which(cmd: &str) -> bool {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if std::path::Path::new(dir).join(cmd).exists() {
                return true;
            }
        }
    }
    false
}

pub async fn run() -> DoctorReport {
    let shells = ["sh", "bash", "zsh", "fish"]
        .iter()
        .map(|s| (s.to_string(), which(s)))
        .collect();
    let profiles = profile::all_names().iter().map(|s| s.to_string()).collect();

    let (ok, detail) = self_test().await;

    DoctorReport {
        muse_version: env!("CARGO_PKG_VERSION").to_string(),
        font_fingerprint: muse_render::font::fingerprint(),
        font_glyphs: muse_render::font::glyph_count(),
        shells,
        profiles,
        self_test_ok: ok,
        self_test_detail: detail,
    }
}

async fn self_test() -> (bool, String) {
    let handle = match spawn_terminal(
        profile::xterm(),
        40,
        10,
        vec!["echo".into(), "muse-doctor-ok".into()],
        HashMap::new(),
        None,
        SyncConfig::default(),
    ) {
        Ok(h) => h,
        Err(e) => return (false, format!("spawn failed: {e}")),
    };
    let res = assert::to_be_visible(
        &handle,
        Locator::Text {
            pattern: "muse-doctor-ok".into(),
            ignore_case: false,
            whole_line: false,
        },
        false,
        3000,
    )
    .await;
    handle.shutdown().await.ok();
    match res {
        Ok(o) if o.ok => (true, "spawn echo + assert visible".into()),
        Ok(o) => (false, o.detail),
        Err(e) => (false, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn doctor_self_test_passes() {
        let r = run().await;
        assert!(r.self_test_ok, "{}", r.self_test_detail);
        assert!(r.profiles.contains(&"xterm".to_string()));
        assert!(r.font_glyphs >= 95);
        let out = r.render();
        assert!(out.contains("self-test: PASS"));
        assert!(out.contains("profiles:"));
    }

    #[test]
    fn which_finds_sh() {
        assert!(which("sh") || which("bash"));
        assert!(!which("definitely-not-a-real-binary-xyz"));
    }
}
