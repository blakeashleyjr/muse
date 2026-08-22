//! `muse session …` / `muse serve`: drive a program interactively across CLI
//! invocations — open it once, then send keys, take screenshots, wait for
//! text, read its output, and close it, each as a separate command. This is
//! the surface an agent uses to check its own work on a TUI.

pub mod client;
pub mod daemon;
pub mod export;
pub mod keys;
pub mod mcp;
pub mod proto;

use crate::Outcome;
use muse_core::error::{Error, Result};
use muse_core::locator::Locator;
use muse_core::snapshot::SnapshotKind;
use proto::{Input, Op, Request, Response, WaitCond};
use std::path::PathBuf;
use std::time::Duration;

/// Default daemon idle exit, in ms, when `muse session open` has to start one.
const DEFAULT_IDLE_MS: u64 = 600_000;

#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// Socket path (default: $MUSE_SOCKET, $XDG_RUNTIME_DIR/muse/muse.sock).
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// Exit after this long with no sessions and no requests.
    #[arg(long, default_value_t = DEFAULT_IDLE_MS)]
    pub idle_ms: u64,
    /// Stop a running daemon (closing its sessions) instead of starting one.
    #[arg(long)]
    pub stop: bool,
}

#[derive(clap::Args, Debug)]
pub struct McpArgs {
    /// Socket path (default: $MUSE_SOCKET, $XDG_RUNTIME_DIR/muse/muse.sock).
    #[arg(long)]
    pub socket: Option<PathBuf>,
}

pub async fn cmd_mcp(a: &McpArgs) -> Outcome {
    let srv = mcp::McpServer::new(mcp::socket_for(a.socket.as_deref()));
    match srv.run_stdio().await {
        Ok(()) => Outcome::ok(""),
        Err(e) => transport(e),
    }
}

#[derive(clap::Args, Debug)]
pub struct SessionArgs {
    /// Socket path (default: $MUSE_SOCKET, $XDG_RUNTIME_DIR/muse/muse.sock).
    #[arg(long, global = true)]
    pub socket: Option<PathBuf>,
    /// Machine-readable output (one JSON document).
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub cmd: SessionCmd,
}

#[derive(clap::Subcommand, Debug)]
pub enum SessionCmd {
    /// Spawn a program in a new session; prints the session id.
    Open(OpenArgs),
    /// Send input: text, key chords, a paste, raw bytes, or a mouse event.
    Send(SendArgs),
    /// Resize the terminal, e.g. `100x30`.
    Resize { id: String, size: String },
    /// Wait for a stable frame and print it (text/styled) or write a PNG.
    Snap(SnapArgs),
    /// The live screen right now, with cursor/title/modes (always JSON).
    Screen { id: String },
    /// Wait until text is (not) on screen, or the program exits.
    Wait(WaitArgs),
    /// Everything the program has written so far (raw bytes, lossy UTF-8).
    Logs {
        id: String,
        /// Print the path of the asciinema cast instead of its contents.
        #[arg(long)]
        cast: bool,
    },
    /// Copy the session's trace (casts, frames, steps) to a directory.
    Trace {
        id: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// List sessions.
    List,
    /// Close a session (or all of them), stopping the program.
    Close {
        id: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Print a `muse run` spec reproducing this session (inputs + the waits
    /// that held), ready to save as a regression test.
    ExportSpec {
        id: String,
        /// Spec name (default: the session name or id).
        #[arg(long)]
        name: Option<String>,
        /// Write to this file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(clap::Args, Debug)]
pub struct OpenArgs {
    #[arg(long, default_value = "xterm")]
    pub profile: String,
    #[arg(long, default_value = "80x24")]
    pub size: String,
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// Extra environment, `KEY=VALUE` (repeatable).
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    /// A memorable alias usable wherever an id is expected.
    #[arg(long)]
    pub name: Option<String>,
    /// Don't record a trace (disables `logs`/`trace`).
    #[arg(long)]
    pub no_trace: bool,
    #[arg(long)]
    pub quiet_ms: Option<u64>,
    #[arg(long)]
    pub max_settle_ms: Option<u64>,
    /// Idle exit for a daemon started by this command.
    #[arg(long, default_value_t = DEFAULT_IDLE_MS)]
    pub idle_ms: u64,
    /// The program and its arguments.
    #[arg(required = true, last = true)]
    pub argv: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct SendArgs {
    pub id: String,
    /// Literal text (use `\n` for Enter via --bytes, or --key enter).
    #[arg(long)]
    pub text: Option<String>,
    /// Key chord like `ctrl+c`, `alt+enter`, `f5`, `x` (repeatable, in order).
    #[arg(long = "key")]
    pub keys: Vec<String>,
    /// Bracketed paste of this text.
    #[arg(long)]
    pub paste: Option<String>,
    /// Raw bytes with `\xNN`, `\e`, `\n` escapes.
    #[arg(long)]
    pub bytes: Option<String>,
    /// Mouse event: `[action:]button[+mods]@row,col`, e.g. `@3,10`,
    /// `release:left@3,10`, `wheel_down@0,0`.
    #[arg(long)]
    pub mouse: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct SnapArgs {
    pub id: String,
    /// text | styled | pixel
    #[arg(long, default_value = "text")]
    pub kind: String,
    #[arg(long, default_value_t = 1)]
    pub scale: u8,
    /// Where to write a pixel PNG (default: the session directory).
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Require this many identical consecutive stable frames.
    #[arg(long, default_value_t = 1)]
    pub min_stable: u8,
    #[arg(long, default_value_t = 3000)]
    pub timeout_ms: u64,
}

#[derive(clap::Args, Debug)]
pub struct WaitArgs {
    pub id: String,
    /// Text that must appear.
    #[arg(long)]
    pub visible: Option<String>,
    /// Regex that must match.
    #[arg(long)]
    pub regex: Option<String>,
    /// Text that must be absent.
    #[arg(long)]
    pub not_visible: Option<String>,
    /// Scope --contains/--equals to one row.
    #[arg(long)]
    pub line: Option<u16>,
    /// The located text must contain this.
    #[arg(long)]
    pub contains: Option<String>,
    /// The located text must equal this.
    #[arg(long)]
    pub equals: Option<String>,
    /// At least this many matches of --visible/--regex.
    #[arg(long)]
    pub count_min: Option<usize>,
    /// Wait for the program to exit.
    #[arg(long)]
    pub exit: bool,
    #[arg(long)]
    pub ignore_case: bool,
    #[arg(long, default_value_t = 5000)]
    pub timeout_ms: u64,
}

fn bad(msg: impl Into<String>) -> Error {
    Error::BadArgument(msg.into())
}

fn parse_env(pairs: &[String]) -> Result<Vec<(String, String)>> {
    pairs
        .iter()
        .map(|p| {
            p.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| bad(format!("--env `{p}`: expected KEY=VALUE")))
        })
        .collect()
}

fn parse_size(s: &str) -> Result<(u16, u16)> {
    muse_runner::spec::parse_size(s).ok_or_else(|| bad(format!("bad size `{s}` (want WxH)")))
}

/// Build the inputs a `send` describes, in a fixed order: text, keys, paste,
/// bytes, mouse.
pub fn send_inputs(a: &SendArgs) -> Result<Vec<Input>> {
    let mut inputs = Vec::new();
    if let Some(t) = &a.text {
        inputs.push(Input::Text { text: t.clone() });
    }
    for k in &a.keys {
        inputs.push(Input::Key {
            key: keys::parse_chord(k)?,
        });
    }
    if let Some(p) = &a.paste {
        inputs.push(Input::Paste { text: p.clone() });
    }
    if let Some(b) = &a.bytes {
        let hex = keys::unescape(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect();
        inputs.push(Input::Bytes { hex });
    }
    if let Some(m) = &a.mouse {
        inputs.push(Input::Mouse {
            ev: keys::parse_mouse(m)?,
        });
    }
    if inputs.is_empty() {
        return Err(bad(
            "send: nothing to send (--text/--key/--paste/--bytes/--mouse)",
        ));
    }
    Ok(inputs)
}

/// Build the wait condition a `wait` describes.
pub fn wait_cond(a: &WaitArgs) -> Result<WaitCond> {
    if a.exit {
        return Ok(WaitCond::Exit);
    }
    let text = |p: &String| Locator::Text {
        pattern: p.clone(),
        ignore_case: a.ignore_case,
        whole_line: false,
    };
    let loc = if let Some(v) = &a.visible {
        Some(text(v))
    } else if let Some(r) = &a.regex {
        Some(Locator::Regex { re: r.clone() })
    } else {
        a.line.map(|l| Locator::Line { row: l })
    };
    if let Some(nv) = &a.not_visible {
        return Ok(WaitCond::NotVisible {
            loc: text(nv),
            multiline: false,
        });
    }
    let loc =
        loc.ok_or_else(|| bad("wait: need one of --visible/--regex/--not-visible/--line/--exit"))?;
    if let Some(eq) = &a.equals {
        return Ok(WaitCond::Text {
            loc,
            equals: eq.clone(),
            multiline: false,
        });
    }
    if let Some(c) = &a.contains {
        return Ok(WaitCond::Contains {
            loc,
            contains: c.clone(),
            multiline: false,
        });
    }
    if let Some(min) = a.count_min {
        return Ok(WaitCond::Count {
            loc,
            eq: None,
            min: Some(min),
            max: None,
            multiline: false,
        });
    }
    Ok(WaitCond::Visible {
        loc,
        multiline: false,
    })
}

/// Exit code 2: the daemon/transport failed (as opposed to 1: the program
/// did something we didn't expect).
fn transport(e: Error) -> Outcome {
    Outcome {
        stdout: format!("error: {e}\n"),
        success: false,
        code: 2,
    }
}

fn json_out(v: &impl serde::Serialize, success: bool) -> Outcome {
    Outcome {
        stdout: format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default()),
        success,
        code: if success { 0 } else { 1 },
    }
}

pub async fn cmd_serve(a: &ServeArgs) -> Outcome {
    let sock = client::socket_path(a.socket.as_deref());
    if a.stop {
        return match client::request(&sock, &Request::new(Op::Shutdown)).await {
            Ok(Response::Closed { closed }) => Outcome::ok(format!(
                "stopped daemon on {} ({} session(s) closed)\n",
                sock.display(),
                closed.len()
            )),
            Ok(other) => transport(Error::Internal(format!("unexpected reply {other:?}"))),
            Err(_) => Outcome::ok(format!("no daemon on {}\n", sock.display())),
        };
    }
    match daemon::serve(&sock, Duration::from_millis(a.idle_ms)).await {
        Ok(true) => Outcome::ok(""),
        Ok(false) => Outcome::ok(format!(
            "another daemon already owns {}\n",
            client::daemon_dir(&sock).display()
        )),
        Err(e) => transport(e),
    }
}

pub async fn cmd_session(a: &SessionArgs) -> Outcome {
    let sock = client::socket_path(a.socket.as_deref());
    let json = a.json;
    // Only `open` starts a daemon; everything else needs one to exist.
    let op = match &a.cmd {
        SessionCmd::Open(o) => {
            if let Err(e) = client::connect_or_spawn(&sock, o.idle_ms).await {
                return transport(e);
            }
            let (cols, rows) = match parse_size(&o.size) {
                Ok(v) => v,
                Err(e) => return transport(e),
            };
            let env = match parse_env(&o.env) {
                Ok(v) => v,
                Err(e) => return transport(e),
            };
            let cwd = o.cwd.clone().map(|c| {
                if c.is_absolute() {
                    c
                } else {
                    std::env::current_dir().map(|d| d.join(&c)).unwrap_or(c)
                }
            });
            Op::Open {
                argv: o.argv.clone(),
                profile: o.profile.clone(),
                cols,
                rows,
                cwd,
                env,
                name: o.name.clone(),
                trace: !o.no_trace,
                quiet_window_ms: o.quiet_ms,
                max_settle_ms: o.max_settle_ms,
            }
        }
        SessionCmd::Send(s) => {
            let inputs = match send_inputs(s) {
                Ok(v) => v,
                Err(e) => return transport(e),
            };
            // Several inputs → several requests, in order; report the last.
            let mut last = Response::Ack;
            for input in inputs {
                match client::request(
                    &sock,
                    &Request::new(Op::Send {
                        id: s.id.clone(),
                        input,
                    }),
                )
                .await
                {
                    Ok(Response::Error { message }) => return transport(Error::Internal(message)),
                    Ok(r) => last = r,
                    Err(e) => return transport(e),
                }
            }
            return render(last, json, &a.cmd);
        }
        SessionCmd::Resize { id, size } => {
            let (cols, rows) = match parse_size(size) {
                Ok(v) => v,
                Err(e) => return transport(e),
            };
            Op::Resize {
                id: id.clone(),
                cols,
                rows,
            }
        }
        SessionCmd::Snap(s) => {
            let kind = match s.kind.as_str() {
                "text" => SnapshotKind::Text,
                "styled" => SnapshotKind::Styled,
                "pixel" | "png" => SnapshotKind::Pixel {
                    scale: s.scale.max(1),
                },
                other => return transport(bad(format!("bad --kind `{other}`"))),
            };
            let out = s.out.clone().map(|p| {
                if p.is_absolute() {
                    p
                } else {
                    std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p)
                }
            });
            Op::Snap {
                id: s.id.clone(),
                kind,
                min_stable: s.min_stable.max(1),
                timeout_ms: s.timeout_ms,
                out,
            }
        }
        SessionCmd::Screen { id } => Op::Screen { id: id.clone() },
        SessionCmd::Wait(w) => {
            let cond = match wait_cond(w) {
                Ok(c) => c,
                Err(e) => return transport(e),
            };
            Op::Wait {
                id: w.id.clone(),
                cond,
                timeout_ms: w.timeout_ms,
            }
        }
        SessionCmd::Logs { id, .. } => Op::Logs { id: id.clone() },
        SessionCmd::Trace { id, out } => {
            let out = if out.is_absolute() {
                out.clone()
            } else {
                std::env::current_dir()
                    .map(|d| d.join(out))
                    .unwrap_or_else(|_| out.clone())
            };
            Op::Trace {
                id: id.clone(),
                out,
            }
        }
        SessionCmd::List => Op::List,
        SessionCmd::Close { id, all } => Op::Close {
            id: id.clone(),
            all: *all,
        },
        SessionCmd::ExportSpec { id, name, .. } => Op::ExportSpec {
            id: id.clone(),
            name: name.clone(),
        },
    };
    match client::request(&sock, &Request::new(op)).await {
        Ok(r) => render(r, json, &a.cmd),
        Err(e) => {
            if matches!(a.cmd, SessionCmd::List) {
                // No daemon ⇒ no sessions. That's an answer, not a failure.
                return if json {
                    json_out(&Response::List { sessions: vec![] }, true)
                } else {
                    Outcome::ok("no sessions (no daemon running)\n")
                };
            }
            transport(e)
        }
    }
}

fn render(resp: Response, json: bool, cmd: &SessionCmd) -> Outcome {
    if let Response::Error { message } = &resp {
        return if json {
            Outcome {
                stdout: format!(
                    "{}\n",
                    serde_json::to_string_pretty(&resp).unwrap_or_default()
                ),
                success: false,
                code: 2,
            }
        } else {
            transport(Error::Internal(message.clone()))
        };
    }
    if json || matches!(cmd, SessionCmd::Screen { .. }) {
        let success = !matches!(resp, Response::Wait { ok: false, .. });
        return json_out(&resp, success);
    }
    match resp {
        Response::Opened { session } => Outcome::ok(format!("{}\n", session.id)),
        Response::Ack | Response::Pong { .. } => Outcome::ok(""),
        Response::Snap { text: Some(t), .. } => Outcome::ok(if t.ends_with('\n') {
            t
        } else {
            format!("{t}\n")
        }),
        Response::Snap {
            png: Some(p),
            width,
            height,
            ..
        } => Outcome::ok(format!(
            "{} ({}x{})\n",
            p.display(),
            width.unwrap_or(0),
            height.unwrap_or(0)
        )),
        Response::Snap { .. } => Outcome::ok(""),
        Response::Wait {
            ok,
            actual,
            expected,
            detail,
            exit_code,
        } => {
            let line = if ok {
                format!("ok: {expected} ({actual})\n")
            } else {
                let ex = exit_code
                    .map(|c| format!(" [program exited with {c}]"))
                    .unwrap_or_default();
                format!("FAIL: expected {expected}, got {actual}: {detail}{ex}\n")
            };
            Outcome {
                stdout: line,
                success: ok,
                code: if ok { 0 } else { 1 },
            }
        }
        Response::Logs { text, cast } => {
            if matches!(cmd, SessionCmd::Logs { cast: true, .. }) {
                Outcome::ok(format!("{}\n", cast.display()))
            } else {
                Outcome::ok(text)
            }
        }
        Response::Trace { dir } => Outcome::ok(format!("{}\n", dir.display())),
        Response::List { sessions } => {
            if sessions.is_empty() {
                return Outcome::ok("no sessions\n");
            }
            let mut s = String::from("ID     NAME        SIZE     PID     STATUS   COMMAND\n");
            for i in sessions {
                s.push_str(&format!(
                    "{:<6} {:<11} {:<8} {:<7} {:<8} {}\n",
                    i.id,
                    i.name.unwrap_or_default(),
                    format!("{}x{}", i.cols, i.rows),
                    i.pid.map(|p| p.to_string()).unwrap_or_default(),
                    i.exit_code
                        .map(|c| format!("exit {c}"))
                        .unwrap_or_else(|| "running".into()),
                    i.argv.join(" ")
                ));
            }
            Outcome::ok(s)
        }
        Response::Closed { closed } => Outcome::ok(format!("closed {}\n", closed.join(" "))),
        Response::Spec { yaml } => {
            if let SessionCmd::ExportSpec { out: Some(p), .. } = cmd {
                return match std::fs::write(p, &yaml) {
                    Ok(()) => Outcome::ok(format!("{}\n", p.display())),
                    Err(e) => transport(Error::Internal(format!("write {}: {e}", p.display()))),
                };
            }
            Outcome::ok(yaml)
        }
        Response::Screen { .. } | Response::Error { .. } => unreachable!("handled above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{dispatch, Cli};
    use clap::Parser;

    #[test]
    fn env_and_inputs_parse() {
        assert_eq!(
            parse_env(&["A=1".into(), "B=x=y".into()]).unwrap(),
            vec![("A".into(), "1".into()), ("B".into(), "x=y".into())]
        );
        assert!(parse_env(&["NOEQ".into()]).is_err());
        let s = SendArgs {
            id: "s1".into(),
            text: Some("a".into()),
            keys: vec!["ctrl+c".into(), "enter".into()],
            paste: Some("p".into()),
            bytes: Some("\\e[A".into()),
            mouse: Some("@1,2".into()),
        };
        let inputs = send_inputs(&s).unwrap();
        assert_eq!(inputs.len(), 6);
        assert!(matches!(&inputs[4], Input::Bytes { hex } if hex == "1b5b41"));
        let empty = SendArgs {
            id: "s1".into(),
            text: None,
            keys: vec![],
            paste: None,
            bytes: None,
            mouse: None,
        };
        assert!(send_inputs(&empty).is_err());
    }

    fn wait_args() -> WaitArgs {
        WaitArgs {
            id: "s".into(),
            visible: None,
            regex: None,
            not_visible: None,
            line: None,
            contains: None,
            equals: None,
            count_min: None,
            exit: false,
            ignore_case: false,
            timeout_ms: 1,
        }
    }

    #[test]
    fn wait_conditions() {
        assert!(wait_cond(&wait_args()).is_err());
        let mut a = wait_args();
        a.exit = true;
        assert_eq!(wait_cond(&a).unwrap(), WaitCond::Exit);
        let mut a = wait_args();
        a.visible = Some("x".into());
        assert!(matches!(wait_cond(&a).unwrap(), WaitCond::Visible { .. }));
        a.count_min = Some(2);
        assert!(matches!(
            wait_cond(&a).unwrap(),
            WaitCond::Count { min: Some(2), .. }
        ));
        let mut a = wait_args();
        a.not_visible = Some("x".into());
        assert!(matches!(
            wait_cond(&a).unwrap(),
            WaitCond::NotVisible { .. }
        ));
        let mut a = wait_args();
        a.line = Some(3);
        a.contains = Some("c".into());
        assert!(matches!(
            wait_cond(&a).unwrap(),
            WaitCond::Contains {
                loc: Locator::Line { row: 3 },
                ..
            }
        ));
        let mut a = wait_args();
        a.regex = Some("r".into());
        a.equals = Some("e".into());
        assert!(matches!(wait_cond(&a).unwrap(), WaitCond::Text { .. }));
    }

    #[test]
    fn cli_parses_session_and_serve() {
        let cli = Cli::try_parse_from([
            "muse", "session", "open", "--size", "100x30", "--env", "A=1", "--name", "n", "--",
            "sh", "-c", "echo",
        ])
        .unwrap();
        match cli.cmd {
            crate::Cmd::Session(s) => match s.cmd {
                SessionCmd::Open(o) => {
                    assert_eq!(o.argv, vec!["sh", "-c", "echo"]);
                    assert_eq!(o.size, "100x30");
                }
                _ => panic!(),
            },
            _ => panic!(),
        }
        let cli =
            Cli::try_parse_from(["muse", "session", "--json", "send", "s1", "--key", "ctrl+c"])
                .unwrap();
        assert!(matches!(
            cli.cmd,
            crate::Cmd::Session(SessionArgs { json: true, .. })
        ));
        let cli = Cli::try_parse_from(["muse", "serve", "--stop", "--socket", "/tmp/x"]).unwrap();
        assert!(matches!(
            cli.cmd,
            crate::Cmd::Serve(ServeArgs { stop: true, .. })
        ));
        assert!(Cli::try_parse_from(["muse", "session", "open"]).is_err());
    }

    #[tokio::test]
    async fn list_without_daemon_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("none.sock").to_string_lossy().into_owned();
        let cli = Cli::try_parse_from(["muse", "session", "--socket", &sock, "list"]).unwrap();
        let o = dispatch(cli).await;
        assert!(o.success);
        assert!(o.stdout.contains("no sessions"));
        let cli =
            Cli::try_parse_from(["muse", "session", "--socket", &sock, "snap", "s1"]).unwrap();
        let o = dispatch(cli).await;
        assert!(!o.success);
        assert_eq!(o.code, 2);
        let cli = Cli::try_parse_from(["muse", "serve", "--stop", "--socket", &sock]).unwrap();
        assert!(dispatch(cli).await.success);
    }
}
