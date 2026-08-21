//! `muse mcp`: a stdio Model Context Protocol server exposing the session
//! verbs as tools, so an agent host (Claude Code, etc.) can drive a TUI
//! without shelling out. Thin: every tool is one [`client::request`].
//! Pixel snapshots come back as an `image` content block.

use super::client;
use super::proto::{Input, Op, Request, Response, WaitCond};
use super::{keys, DEFAULT_IDLE_MS};
use muse_core::error::{Error, Result};
use muse_core::locator::Locator;
use muse_core::snapshot::SnapshotKind;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

pub struct McpServer {
    sock: PathBuf,
}

fn schema(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

fn id_prop() -> Value {
    json!({"type": "string", "description": "Session id (from open) or --name alias"})
}

/// The tool catalogue (also the documentation an agent sees).
pub fn tools() -> Vec<Value> {
    vec![
        json!({"name": "open", "description": "Spawn a program in a PTY session. Returns the session id. Then use send/wait/snap/close.",
            "inputSchema": schema(json!({
                "argv": {"type": "array", "items": {"type": "string"}, "description": "Program and arguments"},
                "cwd": {"type": "string"},
                "env": {"type": "object", "additionalProperties": {"type": "string"}},
                "cols": {"type": "integer", "default": 80}, "rows": {"type": "integer", "default": 24},
                "profile": {"type": "string", "default": "xterm", "description": "xterm | vt220 | kitty | screen | dumb"},
                "name": {"type": "string", "description": "Alias usable instead of the id"}
            }), &["argv"])}),
        json!({"name": "send", "description": "Send input. keys are chords like ctrl+c, alt+enter, f5, x. Fields apply in order: text, keys, paste, bytes, mouse.",
            "inputSchema": schema(json!({
                "id": id_prop(),
                "text": {"type": "string"},
                "keys": {"type": "array", "items": {"type": "string"}},
                "paste": {"type": "string"},
                "bytes": {"type": "string", "description": "Raw bytes with \\\\xNN / \\\\e / \\\\n escapes"},
                "mouse": {"type": "string", "description": "[action:]button[+mods]@row,col e.g. @3,10 or release:left@3,10"}
            }), &["id"])}),
        json!({"name": "resize", "description": "Resize the terminal.",
            "inputSchema": schema(json!({"id": id_prop(), "cols": {"type": "integer"}, "rows": {"type": "integer"}}), &["id", "cols", "rows"])}),
        json!({"name": "snap", "description": "Wait for the screen to settle and return it: kind=text (plain), styled (with attributes), or pixel (PNG image).",
            "inputSchema": schema(json!({
                "id": id_prop(),
                "kind": {"type": "string", "enum": ["text", "styled", "pixel"], "default": "text"},
                "scale": {"type": "integer", "default": 1},
                "timeout_ms": {"type": "integer", "default": 3000}
            }), &["id"])}),
        json!({"name": "screen", "description": "The live screen right now (no settling) with cursor position, title, and terminal modes.",
            "inputSchema": schema(json!({"id": id_prop()}), &["id"])}),
        json!({"name": "wait", "description": "Retry until a condition holds or the deadline passes: visible (text), regex, not_visible, contains/equals (optionally on one line), or exit. Returns ok=false (not an error) when it doesn't hold.",
            "inputSchema": schema(json!({
                "id": id_prop(),
                "visible": {"type": "string"}, "regex": {"type": "string"}, "not_visible": {"type": "string"},
                "line": {"type": "integer"}, "contains": {"type": "string"}, "equals": {"type": "string"},
                "count_min": {"type": "integer"}, "exit": {"type": "boolean"},
                "ignore_case": {"type": "boolean"},
                "timeout_ms": {"type": "integer", "default": 5000}
            }), &["id"])}),
        json!({"name": "logs", "description": "Everything the program has written so far (raw output, lossy UTF-8).",
            "inputSchema": schema(json!({"id": id_prop()}), &["id"])}),
        json!({"name": "list", "description": "List sessions.", "inputSchema": schema(json!({}), &[])}),
        json!({"name": "close", "description": "Stop the program and drop the session.",
            "inputSchema": schema(json!({"id": id_prop(), "all": {"type": "boolean"}}), &[])}),
        json!({"name": "export_spec", "description": "Render this session's inputs and the waits that held as a runnable `muse run` YAML spec (a regression test).",
            "inputSchema": schema(json!({"id": id_prop(), "name": {"type": "string"}, "out": {"type": "string", "description": "Write to this path instead of returning the YAML"}}), &["id"])}),
        json!({"name": "run_spec", "description": "Run muse spec files (YAML) and return the report. Failing cases keep artifacts (final screen, diffs, trace) under artifacts_dir.",
            "inputSchema": schema(json!({
                "specs": {"type": "array", "items": {"type": "string"}},
                "artifacts_dir": {"type": "string", "default": "test-results"},
                "snapshots_dir": {"type": "string", "default": "snapshots"},
                "ci": {"type": "boolean", "default": false},
                "deadline_ms": {"type": "integer", "default": 5000}
            }), &["specs"])}),
    ]
}

fn text_block(t: impl Into<String>) -> Value {
    json!({"type": "text", "text": t.into()})
}

/// Standard base64 (no dependency for one encoder).
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}
fn u(v: &Value, k: &str, d: u64) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(d)
}
fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}
fn need_id(v: &Value) -> Result<String> {
    s(v, "id").ok_or_else(|| Error::BadArgument("missing `id`".into()))
}

impl McpServer {
    pub fn new(sock: PathBuf) -> McpServer {
        McpServer { sock }
    }

    async fn rq(&self, op: Op) -> Result<Response> {
        match client::request(&self.sock, &Request::new(op)).await? {
            Response::Error { message } => Err(Error::Internal(message)),
            r => Ok(r),
        }
    }

    /// Run one tool; returns MCP `content` blocks and whether it's an error.
    pub async fn call(&self, name: &str, args: &Value) -> (Vec<Value>, bool) {
        match self.call_inner(name, args).await {
            Ok(blocks) => (blocks, false),
            Err(e) => (vec![text_block(format!("error: {e}"))], true),
        }
    }

    async fn call_inner(&self, name: &str, a: &Value) -> Result<Vec<Value>> {
        Ok(match name {
            "open" => {
                client::connect_or_spawn(&self.sock, DEFAULT_IDLE_MS).await?;
                let argv: Vec<String> = a
                    .get("argv")
                    .and_then(Value::as_array)
                    .ok_or_else(|| Error::BadArgument("missing `argv`".into()))?
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect();
                let env = a
                    .get("env")
                    .and_then(Value::as_object)
                    .map(|m| {
                        m.iter()
                            .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();
                let r = self
                    .rq(Op::Open {
                        argv,
                        profile: s(a, "profile").unwrap_or_else(|| "xterm".into()),
                        cols: u(a, "cols", 80) as u16,
                        rows: u(a, "rows", 24) as u16,
                        cwd: s(a, "cwd").map(PathBuf::from),
                        env,
                        name: s(a, "name"),
                        trace: true,
                        quiet_window_ms: None,
                        max_settle_ms: None,
                    })
                    .await?;
                match r {
                    Response::Opened { session } => vec![text_block(
                        serde_json::to_string_pretty(&session).unwrap_or_default(),
                    )],
                    other => vec![text_block(format!("{other:?}"))],
                }
            }
            "send" => {
                let id = need_id(a)?;
                let mut inputs = Vec::new();
                if let Some(t) = s(a, "text") {
                    inputs.push(Input::Text { text: t });
                }
                for k in a
                    .get("keys")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    inputs.push(Input::Key {
                        key: keys::parse_chord(k)?,
                    });
                }
                if let Some(p) = s(a, "paste") {
                    inputs.push(Input::Paste { text: p });
                }
                if let Some(raw) = s(a, "bytes") {
                    inputs.push(Input::Bytes {
                        hex: keys::unescape(&raw)
                            .iter()
                            .map(|x| format!("{x:02x}"))
                            .collect(),
                    });
                }
                if let Some(m) = s(a, "mouse") {
                    inputs.push(Input::Mouse {
                        ev: keys::parse_mouse(&m)?,
                    });
                }
                if inputs.is_empty() {
                    return Err(Error::BadArgument("nothing to send".into()));
                }
                let n = inputs.len();
                for input in inputs {
                    self.rq(Op::Send {
                        id: id.clone(),
                        input,
                    })
                    .await?;
                }
                vec![text_block(format!("sent {n} input(s)"))]
            }
            "resize" => {
                self.rq(Op::Resize {
                    id: need_id(a)?,
                    cols: u(a, "cols", 80) as u16,
                    rows: u(a, "rows", 24) as u16,
                })
                .await?;
                vec![text_block("resized")]
            }
            "snap" => {
                let kind = match s(a, "kind").as_deref().unwrap_or("text") {
                    "styled" => SnapshotKind::Styled,
                    "pixel" | "png" => SnapshotKind::Pixel {
                        scale: (u(a, "scale", 1) as u8).max(1),
                    },
                    _ => SnapshotKind::Text,
                };
                let r = self
                    .rq(Op::Snap {
                        id: need_id(a)?,
                        kind,
                        min_stable: 1,
                        timeout_ms: u(a, "timeout_ms", 3000),
                        out: None,
                    })
                    .await?;
                match r {
                    Response::Snap { text: Some(t), .. } => vec![text_block(t)],
                    Response::Snap {
                        png: Some(p),
                        width,
                        height,
                        ..
                    } => {
                        let bytes = std::fs::read(&p)
                            .map_err(|e| Error::Internal(format!("read {}: {e}", p.display())))?;
                        vec![
                            json!({"type": "image", "data": base64(&bytes), "mimeType": "image/png"}),
                            text_block(format!(
                                "{} ({}x{})",
                                p.display(),
                                width.unwrap_or(0),
                                height.unwrap_or(0)
                            )),
                        ]
                    }
                    other => vec![text_block(format!("{other:?}"))],
                }
            }
            "screen" => match self.rq(Op::Screen { id: need_id(a)? }).await? {
                Response::Screen { screen } => vec![text_block(
                    serde_json::to_string_pretty(&screen).unwrap_or_default(),
                )],
                other => vec![text_block(format!("{other:?}"))],
            },
            "wait" => {
                let id = need_id(a)?;
                let text = |p: String| Locator::Text {
                    pattern: p,
                    ignore_case: b(a, "ignore_case"),
                    whole_line: false,
                };
                let cond = if b(a, "exit") {
                    WaitCond::Exit
                } else if let Some(nv) = s(a, "not_visible") {
                    WaitCond::NotVisible {
                        loc: text(nv),
                        multiline: false,
                    }
                } else {
                    let loc = if let Some(v) = s(a, "visible") {
                        text(v)
                    } else if let Some(r) = s(a, "regex") {
                        Locator::Regex { re: r }
                    } else if let Some(l) = a.get("line").and_then(Value::as_u64) {
                        Locator::Line { row: l as u16 }
                    } else {
                        return Err(Error::BadArgument(
                            "need one of visible/regex/not_visible/line/exit".into(),
                        ));
                    };
                    if let Some(eq) = s(a, "equals") {
                        WaitCond::Text {
                            loc,
                            equals: eq,
                            multiline: false,
                        }
                    } else if let Some(c) = s(a, "contains") {
                        WaitCond::Contains {
                            loc,
                            contains: c,
                            multiline: false,
                        }
                    } else if let Some(m) = a.get("count_min").and_then(Value::as_u64) {
                        WaitCond::Count {
                            loc,
                            eq: None,
                            min: Some(m as usize),
                            max: None,
                            multiline: false,
                        }
                    } else {
                        WaitCond::Visible {
                            loc,
                            multiline: false,
                        }
                    }
                };
                match self
                    .rq(Op::Wait {
                        id,
                        cond,
                        timeout_ms: u(a, "timeout_ms", 5000),
                    })
                    .await?
                {
                    r @ Response::Wait { .. } => {
                        vec![text_block(
                            serde_json::to_string_pretty(&r).unwrap_or_default(),
                        )]
                    }
                    other => vec![text_block(format!("{other:?}"))],
                }
            }
            "logs" => match self.rq(Op::Logs { id: need_id(a)? }).await? {
                Response::Logs { text, .. } => vec![text_block(text)],
                other => vec![text_block(format!("{other:?}"))],
            },
            "list" => {
                let r = match client::request(&self.sock, &Request::new(Op::List)).await {
                    Ok(r) => r,
                    Err(_) => Response::List { sessions: vec![] },
                };
                vec![text_block(
                    serde_json::to_string_pretty(&r).unwrap_or_default(),
                )]
            }
            "close" => match self
                .rq(Op::Close {
                    id: s(a, "id"),
                    all: b(a, "all"),
                })
                .await?
            {
                Response::Closed { closed } => {
                    vec![text_block(format!("closed {}", closed.join(" ")))]
                }
                other => vec![text_block(format!("{other:?}"))],
            },
            "export_spec" => match self
                .rq(Op::ExportSpec {
                    id: need_id(a)?,
                    name: s(a, "name"),
                })
                .await?
            {
                Response::Spec { yaml } => {
                    if let Some(out) = s(a, "out") {
                        std::fs::write(&out, &yaml)
                            .map_err(|e| Error::Internal(format!("write {out}: {e}")))?;
                        vec![text_block(format!("wrote {out}"))]
                    } else {
                        vec![text_block(yaml)]
                    }
                }
                other => vec![text_block(format!("{other:?}"))],
            },
            "run_spec" => {
                let specs: Vec<PathBuf> = a
                    .get("specs")
                    .and_then(Value::as_array)
                    .ok_or_else(|| Error::BadArgument("missing `specs`".into()))?
                    .iter()
                    .filter_map(|x| x.as_str().map(PathBuf::from))
                    .collect();
                let args = crate::RunArgs {
                    specs,
                    profile: None,
                    size: None,
                    update_snapshots: false,
                    grep: None,
                    shard: None,
                    retries: 0,
                    workers: 0,
                    reporter: "pretty".into(),
                    snapshots_dir: s(a, "snapshots_dir").unwrap_or_else(|| "snapshots".into()),
                    deadline_ms: u(a, "deadline_ms", 5000),
                    ci: b(a, "ci"),
                    allow_empty: false,
                    case_timeout_ms: 120_000,
                    artifacts: s(a, "artifacts_dir").unwrap_or_else(|| "test-results".into()),
                    trace: "retain-on-failure".into(),
                };
                let o = crate::cmd_run(&args).await;
                if o.success {
                    vec![text_block(o.stdout)]
                } else {
                    return Err(Error::Internal(o.stdout));
                }
            }
            other => return Err(Error::BadArgument(format!("unknown tool `{other}`"))),
        })
    }

    /// Handle one JSON-RPC message; `None` for notifications.
    pub async fn handle(&self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "muse", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Drive terminal programs: open → wait/send/snap → export_spec/close. `wait` returns ok=false rather than erroring when the condition doesn't hold; use `snap` (text) to see why."
            }),
            "notifications/initialized" | "notifications/cancelled" => return None,
            "ping" => json!({}),
            "tools/list" => json!({"tools": tools()}),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let (content, is_error) = self.call(name, &args).await;
                json!({"content": content, "isError": is_error})
            }
            _ => {
                id.as_ref()?;
                return Some(json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": format!("method not found: {method}")}}));
            }
        };
        id.as_ref()?;
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    /// Serve stdin/stdout until EOF.
    pub async fn run_stdio(&self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut lines = BufReader::new(stdin).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(msg) => self.handle(&msg).await,
                Err(e) => Some(json!({"jsonrpc": "2.0", "id": null,
                    "error": {"code": -32700, "message": format!("parse error: {e}")}})),
            };
            if let Some(r) = reply {
                let mut out = r.to_string();
                out.push('\n');
                stdout
                    .write_all(out.as_bytes())
                    .await
                    .map_err(|e| Error::Internal(e.to_string()))?;
                stdout
                    .flush()
                    .await
                    .map_err(|e| Error::Internal(e.to_string()))?;
            }
        }
        Ok(())
    }
}

/// Socket for the MCP server: same resolution as the CLI.
pub fn socket_for(flag: Option<&Path>) -> PathBuf {
    client::socket_path(flag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[tokio::test]
    async fn handshake_tools_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let srv = McpServer::new(dir.path().join("none.sock"));
        let r = srv
            .handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
            .await
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert!(srv
            .handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await
            .is_none());
        let r = srv
            .handle(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
            .await
            .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"open") && names.contains(&"snap") && names.contains(&"run_spec"));
        let r = srv
            .handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "nope"}))
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], -32601);
        // unknown tool → isError, not a protocol error
        let r = srv
            .handle(&json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": {"name": "zzz", "arguments": {}}}))
            .await
            .unwrap();
        assert_eq!(r["result"]["isError"], true);
        // list with no daemon is an empty list
        let r = srv
            .handle(&json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
                "params": {"name": "list", "arguments": {}}}))
            .await
            .unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("sessions"));
    }

    #[tokio::test]
    async fn tools_drive_a_session_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("muse.sock");
        let s2 = sock.clone();
        let server = tokio::spawn(async move {
            super::super::daemon::serve(&s2, std::time::Duration::from_secs(30)).await
        });
        for _ in 0..100 {
            if client::ping(&sock).await.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let srv = McpServer::new(sock.clone());
        let call = |name: &'static str, args: Value| {
            let srv = &srv;
            async move { srv.call(name, &args).await }
        };
        let (c, err) = call(
            "open",
            json!({"argv": ["sh", "-c", "echo first; sleep 0.2; echo LATE; exec cat"], "name": "m", "cols": 40, "rows": 8}),
        )
        .await;
        assert!(!err, "{c:?}");
        let (c, err) = call(
            "wait",
            json!({"id": "m", "visible": "LATE", "timeout_ms": 3000}),
        )
        .await;
        assert!(!err);
        assert!(
            c[0]["text"].as_str().unwrap().contains("\"ok\": true"),
            "{c:?}"
        );
        let (c, err) = call(
            "send",
            json!({"id": "m", "text": "typed", "keys": ["enter"]}),
        )
        .await;
        assert!(!err, "{c:?}");
        let (_, err) = call("wait", json!({"id": "m", "visible": "typed"})).await;
        assert!(!err);
        let (c, err) = call("snap", json!({"id": "m"})).await;
        assert!(!err);
        assert!(c[0]["text"].as_str().unwrap().contains("typed"));
        let (c, err) = call("snap", json!({"id": "m", "kind": "pixel"})).await;
        assert!(!err, "{c:?}");
        assert_eq!(c[0]["type"], "image");
        assert_eq!(c[0]["mimeType"], "image/png");
        assert!(c[0]["data"].as_str().unwrap().starts_with("iVBOR")); // PNG magic in base64
        let (c, err) = call("screen", json!({"id": "m"})).await;
        assert!(!err);
        assert!(c[0]["text"].as_str().unwrap().contains("\"cols\": 40"));
        let (c, err) = call(
            "wait",
            json!({"id": "m", "visible": "absent", "timeout_ms": 100}),
        )
        .await;
        assert!(!err, "a failed wait is a result, not an error");
        assert!(c[0]["text"].as_str().unwrap().contains("\"ok\": false"));
        let (c, err) = call("logs", json!({"id": "m"})).await;
        assert!(!err);
        assert!(c[0]["text"].as_str().unwrap().contains("first"));
        let spec = dir.path().join("m.yaml");
        let (_, err) = call(
            "export_spec",
            json!({"id": "m", "out": spec.to_str().unwrap()}),
        )
        .await;
        assert!(!err);
        assert!(std::fs::read_to_string(&spec)
            .unwrap()
            .contains("expect_visible"));
        let (c, err) = call("close", json!({"id": "m"})).await;
        assert!(!err, "{c:?}");
        let (c, err) = call(
            "run_spec",
            json!({"specs": [spec.to_str().unwrap()], "artifacts_dir": dir.path().join("art").to_str().unwrap(),
                   "snapshots_dir": dir.path().join("snaps").to_str().unwrap()}),
        )
        .await;
        assert!(!err, "{c:?}");
        assert!(c[0]["text"].as_str().unwrap().contains("1 passed"));
        let _ = client::request(&sock, &Request::new(Op::Shutdown)).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), server).await;
    }
}
