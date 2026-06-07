//! Result types and reporters (§19): pretty, junit, json.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AssertionResult {
    pub kind: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotResult {
    pub name: String,
    pub outcome: String,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseResult {
    pub name: String,
    pub profile: String,
    pub cols: u16,
    pub rows: u16,
    pub assertions: Vec<AssertionResult>,
    pub snapshots: Vec<SnapshotResult>,
    pub error: Option<String>,
    #[serde(default)]
    pub flaky: bool,
}

impl CaseResult {
    pub fn passed(&self) -> bool {
        self.error.is_none()
            && self.assertions.iter().all(|a| a.ok)
            && self.snapshots.iter().all(|s| s.passed)
    }

    pub fn case_id(&self) -> String {
        format!(
            "{} [{} {}x{}]",
            self.name, self.profile, self.cols, self.rows
        )
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SuiteResult {
    pub cases: Vec<CaseResult>,
}

impl SuiteResult {
    pub fn passed(&self) -> bool {
        self.cases.iter().all(|c| c.passed())
    }
    pub fn total(&self) -> usize {
        self.cases.len()
    }
    pub fn failures(&self) -> usize {
        self.cases.iter().filter(|c| !c.passed()).count()
    }

    pub fn pretty(&self) -> String {
        let mut s = String::new();
        for c in &self.cases {
            let mark = if c.passed() { "PASS" } else { "FAIL" };
            s.push_str(&format!("[{mark}] {}\n", c.case_id()));
            if let Some(e) = &c.error {
                s.push_str(&format!("       error: {e}\n"));
            }
            for a in &c.assertions {
                if !a.ok {
                    s.push_str(&format!("       ✗ {}: {}\n", a.kind, a.detail));
                }
            }
            for sn in &c.snapshots {
                if !sn.passed {
                    s.push_str(&format!("       ✗ snapshot {}: {}\n", sn.name, sn.outcome));
                } else {
                    s.push_str(&format!("       · snapshot {}: {}\n", sn.name, sn.outcome));
                }
            }
            if c.flaky {
                s.push_str("       (flaky: failed then passed on retry)\n");
            }
        }
        s.push_str(&format!(
            "\n{} passed, {} failed, {} total\n",
            self.total() - self.failures(),
            self.failures(),
            self.total()
        ));
        s
    }

    pub fn json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn junit(&self) -> String {
        let failures = self.failures();
        let mut s = String::new();
        s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        s.push_str(&format!(
            "<testsuites tests=\"{}\" failures=\"{}\">\n",
            self.total(),
            failures
        ));
        s.push_str(&format!(
            "  <testsuite name=\"muse\" tests=\"{}\" failures=\"{}\">\n",
            self.total(),
            failures
        ));
        for c in &self.cases {
            s.push_str(&format!(
                "    <testcase name=\"{}\" classname=\"{}\">\n",
                xml_escape(&c.case_id()),
                xml_escape(&c.profile)
            ));
            if !c.passed() {
                let mut msg = String::new();
                if let Some(e) = &c.error {
                    msg.push_str(e);
                }
                for a in c.assertions.iter().filter(|a| !a.ok) {
                    msg.push_str(&format!("{}: {}; ", a.kind, a.detail));
                }
                for sn in c.snapshots.iter().filter(|s| !s.passed) {
                    msg.push_str(&format!("snapshot {}: {}; ", sn.name, sn.outcome));
                }
                s.push_str(&format!(
                    "      <failure message=\"{}\"></failure>\n",
                    xml_escape(&msg)
                ));
            }
            s.push_str("    </testcase>\n");
        }
        s.push_str("  </testsuite>\n</testsuites>\n");
        s
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(ok: bool) -> CaseResult {
        CaseResult {
            name: "t".into(),
            profile: "xterm".into(),
            cols: 80,
            rows: 24,
            assertions: vec![AssertionResult {
                kind: "toBeVisible".into(),
                ok,
                detail: if ok { "".into() } else { "missing".into() },
            }],
            snapshots: vec![SnapshotResult {
                name: "s".into(),
                outcome: "match".into(),
                passed: true,
            }],
            error: None,
            flaky: false,
        }
    }

    #[test]
    fn passed_logic() {
        assert!(case(true).passed());
        assert!(!case(false).passed());
        let mut c = case(true);
        c.error = Some("boom".into());
        assert!(!c.passed());
    }

    #[test]
    fn suite_counts() {
        let suite = SuiteResult {
            cases: vec![case(true), case(false)],
        };
        assert_eq!(suite.total(), 2);
        assert_eq!(suite.failures(), 1);
        assert!(!suite.passed());
    }

    #[test]
    fn pretty_output() {
        let suite = SuiteResult {
            cases: vec![case(true), case(false)],
        };
        let p = suite.pretty();
        assert!(p.contains("[PASS]"));
        assert!(p.contains("[FAIL]"));
        assert!(p.contains("1 passed, 1 failed"));
    }

    #[test]
    fn junit_valid_ish() {
        let suite = SuiteResult {
            cases: vec![case(false)],
        };
        let x = suite.junit();
        assert!(x.contains("<testsuites"));
        assert!(x.contains("<failure"));
        assert!(x.contains("tests=\"1\""));
    }

    #[test]
    fn json_output() {
        let suite = SuiteResult {
            cases: vec![case(true)],
        };
        let j = suite.json();
        assert!(j.contains("\"profile\""));
    }

    #[test]
    fn xml_escaping() {
        assert_eq!(xml_escape("a&b<c>\""), "a&amp;b&lt;c&gt;&quot;");
    }

    #[test]
    fn case_id_format() {
        assert_eq!(case(true).case_id(), "t [xterm 80x24]");
    }

    #[test]
    fn flaky_note() {
        let mut c = case(true);
        c.flaky = true;
        let suite = SuiteResult { cases: vec![c] };
        assert!(suite.pretty().contains("flaky"));
    }

    #[test]
    fn junit_includes_error_and_snapshot_failure() {
        let mut c = case(false);
        c.error = Some("spawn boom <crash>".into());
        c.snapshots = vec![SnapshotResult {
            name: "s".into(),
            outcome: "mismatch: pixels".into(),
            passed: false,
        }];
        let suite = SuiteResult { cases: vec![c] };
        let x = suite.junit();
        assert!(x.contains("spawn boom &lt;crash&gt;"));
        assert!(x.contains("snapshot s: mismatch"));
    }

    #[test]
    fn pretty_shows_error_line() {
        let mut c = case(true);
        c.error = Some("kaboom".into());
        let suite = SuiteResult { cases: vec![c] };
        assert!(suite.pretty().contains("error: kaboom"));
    }

    #[test]
    fn empty_suite_passes() {
        let suite = SuiteResult::default();
        assert!(suite.passed());
        assert_eq!(suite.total(), 0);
        assert!(suite.junit().contains("tests=\"0\""));
    }
}
