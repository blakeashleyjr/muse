//! Wire protocol between `muse session …` (client) and `muse serve` (daemon):
//! one newline-delimited JSON request, one newline-delimited JSON response,
//! per connection. Long operations (`wait`, `snap`) simply hold the connection.
//!
//! The shapes are pinned by round-trip tests; bump [`PROTOCOL_VERSION`] when
//! a change would make an older client misread a newer daemon.

use muse_core::cursor::Cursor;
use muse_core::input::{KeyEvent, MouseEvent};
use muse_core::locator::Locator;
use muse_core::modes::ModeState;
use muse_core::snapshot::SnapshotKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    #[serde(flatten)]
    pub op: Op,
}

impl Request {
    pub fn new(op: Op) -> Request {
        Request {
            v: PROTOCOL_VERSION,
            op,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Ping,
    Open {
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
    },
    Send {
        id: String,
        input: Input,
    },
    Resize {
        id: String,
        cols: u16,
        rows: u16,
    },
    /// Wait for a stable frame (or the deadline) and render it.
    Snap {
        id: String,
        kind: SnapshotKind,
        min_stable: u8,
        timeout_ms: u64,
        /// Where the daemon writes a pixel PNG; defaults to the session dir.
        out: Option<PathBuf>,
    },
    /// The live screen right now (no settling): cursor, title, modes, text.
    Screen {
        id: String,
    },
    Wait {
        id: String,
        cond: WaitCond,
        timeout_ms: u64,
    },
    /// Raw SUT output so far (from the trace's output cast).
    Logs {
        id: String,
    },
    /// Flush and copy the session's trace directory to `out`.
    Trace {
        id: String,
        out: PathBuf,
    },
    List,
    Close {
        id: Option<String>,
        all: bool,
    },
    /// Render everything done to this session so far as a runnable spec.
    ExportSpec {
        id: String,
        /// Spec `name:`; defaults to the session name or id.
        name: Option<String>,
    },
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Input {
    Text {
        text: String,
    },
    /// Raw bytes, hex-encoded (for escape sequences that have no key name).
    Bytes {
        hex: String,
    },
    Key {
        key: KeyEvent,
    },
    Paste {
        text: String,
    },
    Mouse {
        ev: MouseEvent,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WaitCond {
    Visible {
        loc: Locator,
        multiline: bool,
    },
    NotVisible {
        loc: Locator,
        multiline: bool,
    },
    Text {
        loc: Locator,
        equals: String,
        multiline: bool,
    },
    Contains {
        loc: Locator,
        contains: String,
        multiline: bool,
    },
    Count {
        loc: Locator,
        eq: Option<usize>,
        min: Option<usize>,
        max: Option<usize>,
        multiline: bool,
    },
    Exit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub name: Option<String>,
    pub argv: Vec<String>,
    pub profile: String,
    pub cols: u16,
    pub rows: u16,
    pub pid: Option<u32>,
    pub exit_code: Option<u32>,
    pub opened_at: u64,
    pub dir: PathBuf,
    pub trace: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScreenInfo {
    pub cols: u16,
    pub rows: u16,
    pub cursor: Cursor,
    pub title: Option<String>,
    pub modes: ModeState,
    pub alt_screen: bool,
    pub generation: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Error {
        message: String,
    },
    Pong {
        version: String,
        protocol: u32,
        pid: u32,
        sessions: usize,
    },
    Opened {
        session: SessionInfo,
    },
    Ack,
    Snap {
        /// Text / styled canonical form; `None` for pixel.
        text: Option<String>,
        png: Option<PathBuf>,
        width: Option<u32>,
        height: Option<u32>,
        generation: u64,
    },
    Screen {
        screen: ScreenInfo,
    },
    Wait {
        ok: bool,
        actual: String,
        expected: String,
        detail: String,
        exit_code: Option<u32>,
    },
    Logs {
        text: String,
        cast: PathBuf,
    },
    Trace {
        dir: PathBuf,
    },
    List {
        sessions: Vec<SessionInfo>,
    },
    Closed {
        closed: Vec<String>,
    },
    Spec {
        yaml: String,
    },
}

impl Response {
    pub fn error(msg: impl Into<String>) -> Response {
        Response::Error {
            message: msg.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::input::{Key, Mods, MouseAction, MouseButton};

    fn rt<T: Serialize + for<'a> Deserialize<'a> + PartialEq + std::fmt::Debug>(v: T) {
        let s = serde_json::to_string(&v).unwrap();
        let back: T = serde_json::from_str(&s).unwrap();
        assert_eq!(v, back, "{s}");
    }

    #[test]
    fn requests_round_trip() {
        let loc = Locator::Text {
            pattern: "x".into(),
            ignore_case: false,
            whole_line: false,
        };
        for op in [
            Op::Ping,
            Op::Open {
                argv: vec!["sh".into()],
                profile: "xterm".into(),
                cols: 80,
                rows: 24,
                cwd: None,
                env: vec![("A".into(), "b".into())],
                name: Some("n".into()),
                trace: true,
                quiet_window_ms: Some(50),
                max_settle_ms: None,
            },
            Op::Send {
                id: "s1".into(),
                input: Input::Key {
                    key: KeyEvent::with(Key::Char('c'), Mods::CTRL),
                },
            },
            Op::Send {
                id: "s1".into(),
                input: Input::Mouse {
                    ev: MouseEvent {
                        button: MouseButton::Left,
                        action: MouseAction::Press,
                        row: 1,
                        col: 2,
                        mods: Mods::empty(),
                    },
                },
            },
            Op::Resize {
                id: "s1".into(),
                cols: 1,
                rows: 2,
            },
            Op::Snap {
                id: "s1".into(),
                kind: SnapshotKind::Pixel { scale: 2 },
                min_stable: 1,
                timeout_ms: 10,
                out: Some("/tmp/x.png".into()),
            },
            Op::Screen { id: "s1".into() },
            Op::Wait {
                id: "s1".into(),
                cond: WaitCond::Count {
                    loc: loc.clone(),
                    eq: None,
                    min: Some(1),
                    max: None,
                    multiline: false,
                },
                timeout_ms: 5,
            },
            Op::Logs { id: "s1".into() },
            Op::Trace {
                id: "s1".into(),
                out: "/tmp/t".into(),
            },
            Op::List,
            Op::Close {
                id: None,
                all: true,
            },
            Op::ExportSpec {
                id: "s1".into(),
                name: None,
            },
            Op::Shutdown,
        ] {
            rt(Request::new(op));
        }
    }

    #[test]
    fn responses_round_trip() {
        let info = SessionInfo {
            id: "s1".into(),
            name: None,
            argv: vec![],
            profile: "xterm".into(),
            cols: 80,
            rows: 24,
            pid: Some(1),
            exit_code: None,
            opened_at: 0,
            dir: "/tmp".into(),
            trace: false,
        };
        for r in [
            Response::error("boom"),
            Response::Pong {
                version: "0".into(),
                protocol: 1,
                pid: 2,
                sessions: 0,
            },
            Response::Opened {
                session: info.clone(),
            },
            Response::Ack,
            Response::Snap {
                text: Some("t".into()),
                png: None,
                width: None,
                height: None,
                generation: 3,
            },
            Response::Screen {
                screen: ScreenInfo {
                    cols: 1,
                    rows: 1,
                    cursor: Cursor::default(),
                    title: None,
                    modes: ModeState::default(),
                    alt_screen: false,
                    generation: 0,
                    text: String::new(),
                },
            },
            Response::Wait {
                ok: true,
                actual: "a".into(),
                expected: "e".into(),
                detail: String::new(),
                exit_code: None,
            },
            Response::Logs {
                text: "x".into(),
                cast: "/c".into(),
            },
            Response::Trace { dir: "/d".into() },
            Response::List {
                sessions: vec![info],
            },
            Response::Closed {
                closed: vec!["s1".into()],
            },
            Response::Spec {
                yaml: "name: x".into(),
            },
        ] {
            rt(r);
        }
    }

    #[test]
    fn error_shape_is_stable() {
        let s = serde_json::to_string(&Response::error("x")).unwrap();
        assert_eq!(s, r#"{"result":"error","message":"x"}"#);
    }
}
