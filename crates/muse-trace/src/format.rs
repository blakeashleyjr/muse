//! Trace record types (§13).

use muse_core::screen::Screen;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceMeta {
    pub version: u32,
    pub profile: String,
    pub cols: u16,
    pub rows: u16,
    pub env: Vec<(String, String)>,
    pub started_at: u64,
    pub sut_argv: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameRecord {
    pub ts: f64,
    pub gen: u64,
    pub step_id: u64,
    pub screen: Screen,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssertionRecord {
    pub kind: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    pub step_id: u64,
    pub name: String,
    pub t0: f64,
    pub t1: f64,
    pub assertions: Vec<AssertionRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_roundtrip() {
        let m = TraceMeta {
            version: 1,
            profile: "xterm".into(),
            cols: 80,
            rows: 24,
            env: vec![("TERM".into(), "xterm-256color".into())],
            started_at: 1000,
            sut_argv: vec!["echo".into(), "hi".into()],
        };
        let j = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<TraceMeta>(&j).unwrap(), m);
    }

    #[test]
    fn frame_roundtrip() {
        let f = FrameRecord {
            ts: 1.5,
            gen: 3,
            step_id: 1,
            screen: Screen::new(2, 2),
        };
        let j = serde_json::to_string(&f).unwrap();
        assert_eq!(serde_json::from_str::<FrameRecord>(&j).unwrap(), f);
    }

    #[test]
    fn step_roundtrip() {
        let s = StepRecord {
            step_id: 1,
            name: "login".into(),
            t0: 0.0,
            t1: 1.0,
            assertions: vec![AssertionRecord {
                kind: "toBeVisible".into(),
                ok: true,
                detail: "".into(),
            }],
        };
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<StepRecord>(&j).unwrap(), s);
    }
}
