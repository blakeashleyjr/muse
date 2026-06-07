Proscenium — End-to-End Implementation Design Document

Status: implementation-ready spec • Target: a coding agent building bottom-up • Core: Rust • SDKs: generated (Go, TS/JS, Python, C++, Rust)

This document is written to be executed by an agent with no further clarification. Every component has a contract, the hard algorithms are specified concretely, and each milestone has a binary acceptance test. Build in the dependency order in §26. When a default is given (e.g. quiet_window = 50ms), treat it as authoritative unless a config overrides it.
Contents

    Goals / non-goals / success criteria
    Glossary
    Architecture & crate DAG
    Repository layout
    pros-core domain types
    Emulator subsystem
    PTY subsystem
    Synchronizer (quiescence)
    Input encoding
    Locator engine
    Rendering (text/styled/pixel)
    Diff subsystem
    Trace subsystem
    Actor system
    Protocol + engine server
    Codegen pipeline
    Embedded FFI
    SDK sugar contract
    Runner
    Conformance harness
    Error model
    Configuration
    Observability
    Security
    Build / CI / release
    Implementation plan (milestones + acceptance)
    Edge-case catalog
    Deferred decisions

1. Goals / non-goals / success criteria

Goals. A black-box e2e + visual-regression testing system for terminal programs: spawn a program under a PTY, drive it (keys/mouse/resize/paste), query a faithful screen model with web-first retrying assertions, and snapshot it at three fidelity tiers. Multi-emulation-profile, multi-shell, multi-dimension matrix. Language-agnostic via a generated-SDK wire protocol, plus an embedded FFI mode. Full tracing with a scrubbable viewer.

Non-goals (v1). GUI/graphical-protocol terminals (Sixel/Kitty-graphics rendering correctness is out, though byte passthrough is in). Windows ConPTY is supported via portable-pty but is a P2 conformance target. Freezing a black-box program's wall clock without a shim (offered as opt-in libfaketime + masking, not guaranteed).

Success criteria. (a) pros can run a matrix suite against a real TUI and produce deterministic pass/fail; (b) the same test source compiles/runs through ≥2 generated SDKs (Rust + Go) with identical results on the conformance corpus; (c) pixel snapshots are byte-identical across Linux and macOS CI for the embedded font.
2. Glossary

    SUT — system under test (the spawned terminal program).
    Grid — rows × cols matrix of Cells; the queryable "DOM".
    Cell — one display position: a grapheme cluster + style. Wide glyphs occupy two columns; the trailing column is a Spacer.
    Screen — primary grid + alternate grid + scrollback + cursor + mode state.
    Profile — an emulation personality: capability table + behavior flags + query responses.
    Context — isolation unit: one PTY + one emulator + one synchronizer + one trace. Cheap (~ms).
    Frame — a screen state emitted after the emulator processes a chunk and the synchronizer evaluates stability.
    Quiescence — heuristic "render finished" condition.
    Step — a named span in a test that groups frames/assertions for tracing.

3. Architecture & crate DAG

Strict layering. pros-core is pure domain (no tokio, no proto, no I/O). Build bottom-up; a crate may only depend on crates above it in this list:

pros-core            (types, traits, algorithms; no I/O, no async)
  pros-emulator      (depends: core)            — Emulator trait + backends + profiles
  pros-render        (depends: core)            — text/styled/pixel renderers
  pros-diff          (depends: core, render)    — mask/normalize/perceptual
  pros-trace         (depends: core)            — trace format + asciinema
pros-pty             (depends: core)            — portable-pty wrapper (async via tokio)
pros-engine-actor    (depends: all above + tokio) — Terminal actor, Session/Context mgr
pros-proto           (depends: core)            — prost/tonic generated + From/Into mappers
pros-engine          (depends: actor + proto)  — tonic server, request handlers
pros-ffi             (depends: actor)           — #[no_mangle] C ABI (embedded mode)
pros-sdk             (depends: proto)           — native Rust DSL (sugar over generated client)
pros-runner          (depends: sdk)             — matrix, scheduling, reporters
pros-viewer          (depends: trace, ratatui)  — TUI trace viewer
pros-cli             (depends: engine, runner, viewer) — `pros` binary

Rule: protobuf types never appear above pros-proto; the engine maps proto↔domain at its boundary.
4. Repository layout

proscenium/
  Cargo.toml                      # workspace
  rust-toolchain.toml             # pin toolchain (e.g. 1.79)
  proto/
    proscenium/v1/proscenium.proto
    buf.yaml  buf.gen.yaml  buf.lock
  crates/
    pros-core/        src/{lib,grid,cell,style,color,screen,modes,cursor,locator,snapshot,error,config}.rs
    pros-emulator/    src/{lib,emulator,profile,capabilities,backend_alacritty,query,reduce_color}.rs
    pros-render/      src/{lib,text,styled,pixel,font}.rs
                      assets/font.ttf
    pros-diff/        src/{lib,mask,normalize,perceptual,stabilize,report}.rs
    pros-trace/       src/{lib,format,asciinema,recorder}.rs
    pros-pty/         src/{lib,spawn,winsize,remote}.rs
    pros-engine-actor/src/{lib,terminal,session,context,supervisor,sync}.rs
    pros-proto/       build.rs src/{lib,map}.rs
    pros-engine/      src/{lib,server,handlers,stream}.rs
    pros-ffi/         cbindgen.toml src/{lib,handles,abi}.rs
    pros-sdk/         src/{lib,test,expect,locator,terminal,matrix,reporter}.rs
    pros-runner/      src/{lib,schedule,retry,report_junit,report_pretty}.rs
    pros-viewer/      src/{lib,app,timeline,render_cell}.rs
    pros-cli/         src/{main,cmd_run,cmd_serve,cmd_codegen,cmd_trace,cmd_doctor,cmd_update}.rs
  sdks/
    go/ ts/ python/ cpp/          # generated base + hand-written sugar
  conformance/
    emulator/*.yaml protocol/*.yaml runner.rs
  ffi-headers/proscenium.h        # cbindgen output, checked in
  examples/

5. pros-core domain types

Authoritative type definitions. Use compact_str::CompactString for graphemes, bitflags for attrs, smallvec for style runs.
rust

// color.rs
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Color { Default, Indexed(u8), Rgb(u8, u8, u8) }

// style.rs
bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
    pub struct Attrs: u16 {
        const BOLD=1; const DIM=2; const ITALIC=4; const UNDERLINE=8;
        const BLINK=16; const REVERSE=32; const HIDDEN=64; const STRIKE=128;
        const DOUBLE_UNDERLINE=256; const CURLY_UNDERLINE=512;
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct CellStyle { pub fg: Color, pub bg: Color, pub underline: Color, pub attrs: Attrs }

// cell.rs
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CellKind { Empty, Glyph(CompactString), Spacer /* trailing col of a wide glyph */ }
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cell { pub kind: CellKind, pub style: CellStyle }
impl Cell { pub fn width(&self) -> u8 { /* Spacer=0, Empty/Glyph=display width */ } }

// cursor.rs
#[derive(Clone, Copy, Debug)]
pub struct Cursor { pub row: u16, pub col: u16, pub visible: bool, pub shape: CursorShape }

// modes.rs — mirror the SUT's negotiated modes; input encoders read this
#[derive(Clone, Debug, Default)]
pub struct ModeState {
    pub app_cursor_keys: bool,      // DECCKM
    pub app_keypad: bool,
    pub bracketed_paste: bool,      // 2004
    pub mouse: MouseMode,           // Off | X10 | Normal | ButtonEvent | AnyEvent
    pub mouse_encoding: MouseEnc,   // Default | Utf8 | Sgr(1006) | Urxvt
    pub sync_output: bool,          // 2026 in-progress
    pub kitty_kbd_flags: u8,
    pub alt_screen: bool,
}

// grid.rs
#[derive(Clone)]
pub struct Grid { rows: u16, cols: u16, cells: Vec<Cell> /* row-major, len = rows*cols */ }
impl Grid {
    pub fn cell(&self, r: u16, c: u16) -> &Cell;
    pub fn row_text(&self, r: u16) -> String;     // joins graphemes; Spacer contributes "" 
    pub fn dims(&self) -> (u16, u16);
}

// screen.rs
#[derive(Clone)]
pub struct Screen {
    pub primary: Grid, pub alt: Grid, pub active: ScreenKind,
    pub scrollback: Vec<Vec<Cell>>,  // bounded ring; newest last
    pub cursor: Cursor, pub modes: ModeState, pub title: Option<String>,
}
impl Screen { pub fn active_grid(&self) -> &Grid; }

// locator.rs (see §10)  snapshot.rs (see §11)  error.rs (see §21)  config.rs (see §22)

Width rule (authoritative). Use unicode-width for display width; unicode-segmentation for grapheme clustering. A grapheme of width 2 writes a Glyph at column c and a Spacer at c+1. Zero-width joiners/combining marks attach to the preceding Glyph's CompactString (do not advance the column). Width-0 leading clusters are dropped.
6. Emulator subsystem (pros-emulator)

Decision: v1 wraps alacritty_terminal::Term as the single backend; profiles are a shaping layer over it (env/terminfo setup, query-response rewriting, color reduction). True divergent backends (strict VT220 quirks) are deferred to M3+. This is the tractable, correct path; do not hand-roll a screen model in v1.
rust

pub trait Emulator: Send {
    fn advance(&mut self, bytes: &[u8]);            // feed SUT output
    fn snapshot_screen(&self) -> Screen;            // build domain Screen from backend grid
    fn capabilities(&self) -> &Capabilities;
    fn drain_replies(&mut self) -> Vec<u8>;         // bytes to write BACK to the pty (DA/DSR/DECRQM)
    fn resize(&mut self, cols: u16, rows: u16);
    fn modes(&self) -> &ModeState;                  // kept in sync as bytes are parsed
}

AlacrittyBackend. Holds Term<NoopListener>, an ansi::Processor, and a parsed-out ModeState. advance runs bytes through the processor; query responses Alacritty would emit go through the EventListener::PtyWrite callback — capture them into a replies buffer, then rewrite per profile (e.g. for a vt100 profile, override the DA1 response). snapshot_screen iterates term.grid() mapping each alacritty_terminal::term::cell::Cell → domain Cell (map Flags::WIDE_CHAR→Glyph+Spacer, WIDE_CHAR_SPACER→Spacer, colors via the palette, flags→Attrs), reads term.grid().cursor, and detects alt-screen via Alacritty's mode. Keep a hand-maintained ModeState by also scanning for the mode-set/reset CSI sequences during advance (Alacritty tracks some but not all of what input encoding needs, e.g. bracketed paste and mouse encoding).

Profile / Capabilities.
rust

pub struct Capabilities {
    pub terminfo_name: &'static str,         // "xterm-256color", "vt220", "screen"
    pub color: ColorDepth,                   // NoColor | Ansi16 | Indexed256 | TrueColor
    pub width_mode: WidthMode,               // EastAsianAmbiguousNarrow | Wide
    pub keyboard: KeyboardProtocol,          // Legacy | ModifyOtherKeys | Kitty
    pub mouse: &'static [MouseMode],
    pub supports_sync_output: bool,          // DEC 2026
    pub supports_bracketed_paste: bool,
    pub tab_width: u8,
    pub da1: &'static [u8], pub da2: &'static [u8],
}
pub struct Profile { pub name: &'static str, pub caps: Capabilities, pub env: &'static [(&'static str,&'static str)] }

Built-in profiles (M3): xterm (xterm-256color, TrueColor, ModifyOtherKeys, SGR mouse, 2026, paste), vt220 (Ansi16, Legacy kbd, no mouse, no paste, DA1=\x1b[?62;1;2;6;8;9c), kitty (TrueColor, Kitty kbd, all mouse, 2026), screen (Indexed256, no 2026), dumb (NoColor, Legacy). Color reduction (reduce_color.rs): TrueColor→256 via 6×6×6 cube + grayscale nearest; 256→16 via a fixed LUT; NoColor drops color, keeps attrs. Applied in snapshot_screen when caps.color < TrueColor.

Acceptance. Feed \x1b[31mHELLO\x1b[0m → cell (0,0..5) fg=Indexed(1) under xterm; under dumb, fg=Default, text intact. Feed DA1 query \x1b[c → drain_replies() returns the profile's da1.
7. PTY subsystem (pros-pty)

Wrap portable_pty. Provide async read via a blocking-reader thread → tokio::sync::mpsc of Bytes (portable-pty readers are blocking; bridge with spawn_blocking or a dedicated thread). Spawn API:
rust

pub struct SpawnOpts { pub argv: Vec<String>, pub env: HashMap<String,String>,
    pub cwd: Option<PathBuf>, pub cols: u16, pub rows: u16 }
pub struct Pty { /* master writer, reader rx, child, pid */ }
impl Pty {
    pub fn spawn(opts: SpawnOpts) -> Result<Pty>;
    pub async fn read(&mut self) -> Option<Bytes>;     // None on EOF
    pub fn write(&self, bytes: &[u8]) -> Result<()>;
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()>;  // sets winsize; portable-pty sends SIGWINCH
    pub fn kill(&mut self) -> Result<()>;              // kill process group
    pub fn wait(&mut self) -> ExitStatus;
}

Set TERM from Profile.caps.terminfo_name, plus COLORTERM=truecolor only when TrueColor, and merge Profile.env. Process-group kill (Unix): spawn with a new session/pgid so kill(-pgid) reaps children. remote.rs (P2): same trait over an SSH/exec transport.
8. Synchronizer (pros-engine-actor::sync)

Deterministic "render complete" detection. Owned by the Terminal actor. State machine:

            action() arms                 byte chunk
   Idle ───────────────────► Armed ──────────────────► Receiving
     ▲                          │ quiet_window elapsed     │ each chunk resets timer
     │ Stable consumed          ▼                          │
     └──────────────────────  Stable ◄─────────────────────┘  (timer fires, no sync-block open)

Rules, evaluated on every output chunk and on a timer tick:

    On any output chunk: record last_activity = now; if a DEC-2026 BSU (CSI ? 2026 h) was seen and no ESU (CSI ? 2026 l) yet, set sync_open = true (suppress Stable). On ESU, clear it.
    Stable is declared when: sync_open == false AND now - last_activity >= quiet_window (default 50 ms). Emit a FrameEvent { screen, generation }.
    Cooperative readiness: a private OSC OSC 5379 ; pros:ready ST (\x1b]5379;pros:ready\x07) declares Stable immediately for the current step (use a vendor-unique number; document it). The synchronizer recognizes it during scan and short-circuits the timer.
    Deadline polling: locator/snapshot requests register a waiter (predicate, deadline, oneshot). On each FrameEvent (and a 10 ms safety tick), evaluate waiters; resolve on success or at deadline (returning empty/timeout).

Config: quiet_window (50 ms), max_settle (cap total wait before forcing Stable, 2 s), tick (10 ms). Acceptance: a program that prints "loading" then after 30 ms prints "done" must yield a single Stable frame containing "done" with quiet_window=50ms; a program using BSU/ESU around a multi-write update must not emit an intermediate Stable.
9. Input encoding (pros-core::input, used by actor)

Pure functions; read ModeState from the emulator. Keys → bytes must honor mode:
rust

pub enum Key { Char(char), Enter, Tab, Backspace, Escape, Up, Down, Left, Right,
    Home, End, PageUp, PageDown, Insert, Delete, F(u8) }
pub struct KeyEvent { pub key: Key, pub mods: Mods /* CTRL|ALT|SHIFT|SUPER */ }
pub fn encode_key(ev: &KeyEvent, modes: &ModeState, caps: &Capabilities) -> Vec<u8>;

Encoding table (legacy mode): arrows = ESC O A/B/C/D when app_cursor_keys, else ESC [ A/B/C/D. Ctrl+Char = char & 0x1f. Alt+X = ESC + X. F1–F4 = ESC O P/Q/R/S; F5–F12 = ESC [ 15~ … 24~. Home/End/PgUp/PgDn/Ins/Del = ESC [ 1~/4~/5~/6~/2~/3~. Modified keys with ModifyOtherKeys or Kitty: emit the CSU/CSI-u form per the negotiated protocol (implement Kitty CSI unicode ; modifiers u only when caps.keyboard == Kitty and the SUT enabled it). Mouse: encode_mouse(ev, modes) emits SGR-1006 (ESC [ < b ; col ; row M/m) when mouse_encoding == Sgr and mouse enabled; no-op when disabled. Resize: actor calls pty.resize (not a byte sequence). Paste: if bracketed_paste, wrap in ESC[200~ … ESC[201~, else raw.

Acceptance: encode_key(Up, app_cursor_keys=true) == ESC O A; ==false → ESC [ A. Ctrl+C → [0x03]. Mouse click at row3/col5 SGR → ESC[<0;5;3M.
10. Locator engine (pros-core::locator)
rust

pub enum Locator {
    Text { pattern: String, ignore_case: bool, whole_line: bool },
    Regex { re: String },                 // compiled with `regex`
    Cell { row: u16, col: u16 },
    Region { rect: Rect },
    Styled { text: Option<String>, pred: StylePredicate }, // e.g. fg==Red
    Cursor,
    Line { row: u16 },
}
pub struct Match { pub rect: Rect, pub text: String, pub styles: Vec<(Rect, CellStyle)> }
pub fn resolve(screen: &Screen, loc: &Locator, multiline: bool) -> Vec<Match>;

Algorithm for Text/Regex: build per-row logical strings from active_grid (skip Spacer, map Empty→space). If multiline, also build soft-wrap-joined logical lines (a row whose last cell wrapped joins the next). Search each line; for each hit compute the Rect by walking columns counting display width back to cell coordinates (so a match after a wide glyph maps to the right column). Styled filters matched cells by pred. Cursor returns a 1×1 rect at screen.cursor. Resolution is stateless and pure; the actor wraps it in deadline polling (§8 rule 4) for web-first retry.

Acceptance: with 日本x on row 0 (日,本 wide), Text{"x"} → rect at col 4 (0-indexed). Regex{"^\\$ "} matches a prompt only at line start.
11. Rendering (pros-render)
rust

pub enum SnapshotKind { Text, Styled, Pixel { scale: u8 } }
pub enum Snapshot { Text(String), Styled(StyledSnapshot), Pixel(PixelSnapshot) }
pub trait Renderer { fn render(&self, s: &Screen, k: SnapshotKind) -> Snapshot; }

Text tier: join active grid rows, trim trailing spaces per line, strip trailing blank lines; this is the golden text. Styled tier: a stable, diff-friendly format — for each row, the text line, plus a parallel run-length list of (start_col, len, fg, bg, attrs) serialized canonically (sorted, lowercase hex). Also expose an SVG renderer for human review (cells as <rect>+<text>, embedded font). Pixel tier: deterministic rasterization. Embed one font (assets/font.ttf, a fixed monospace) compiled into the binary via include_bytes!; rasterize with swash/cosmic-text (or fontdue) at fixed cell metrics cell_w × cell_h (e.g. 9×18 at scale=1), fixed palette for the 16 ANSI colors, no system fonts, no subpixel AA (grayscale AA only, or none for max determinism). Output RGBA PNG via image. Determinism mandate: identical Screen + profile ⇒ byte-identical PNG on every OS.

Acceptance: render the same screen twice → identical bytes; render on Linux and macOS CI → identical SHA-256 (CI test).
12. Diff subsystem (pros-diff)
rust

pub struct DiffOptions { pub masks: Vec<MaskRule>, pub normalize: Vec<NormalizeRule>,
    pub pixel_tolerance: u8 /*max per-channel delta*/, pub max_diff_ratio: f32,
    pub stabilize: StabilizeMode }
pub enum MaskRule { Rect(Rect), Content(String /*regex*/) }
pub struct NormalizeRule { pub re: String, pub replace: String } // applied to text/styled pre-diff
pub enum StabilizeMode { Off, RequireStableFrames(u8), AutoMaskVolatile { window: Duration } }
pub enum DiffResult { Match, Mismatch { report: DiffReport } }

Text/Styled diff: apply normalize (regex replace) then masks (replace masked cells with sentinel \u{2588} or blank) to both baseline and actual; compare; on mismatch produce a unified line diff (similar crate) and, for styled, a per-cell style diff list. Pixel diff: apply rect masks (fill constant), compute per-pixel max_channel_delta; a pixel "differs" if delta > pixel_tolerance; diff_ratio = differing/total; Mismatch iff diff_ratio > max_diff_ratio (defaults tolerance=0, ratio=0.0). Emit a diff PNG highlighting changed pixels. Animation stabilization: AutoMaskVolatile collects frames within window, computes the set of cells that change across them, and masks that set before diffing; RequireStableFrames(k) requires k identical consecutive frames before snapshotting (the actor enforces this). Baselines: stored at snapshots/{test}/{profile}__{cols}x{rows}__{os}.{txt|styled|png}; missing baseline ⇒ create + pass on first run (record "created"); --update overwrites.

Acceptance: a screen with a live clock at a known rect passes when that rect is masked; pixel diff of a 1-cell color change with tolerance=0 fails and the diff PNG marks exactly those pixels.
13. Trace subsystem (pros-trace)

A trace is a directory (zipped on export):

trace/
  meta.json        {version, profile, cols, rows, env, started_at, sut_argv}
  input.cast       asciinema v2: header + ["i", ts, data] lines (bytes we wrote)
  output.cast      asciinema v2: header + [ts, "o", data] lines (bytes SUT emitted)
  frames.jsonl     {ts, gen, step_id, screen}  (screen = compact styled snapshot)
  steps.jsonl      {step_id, name, t0, t1, assertions:[{kind, ok, detail}]}
  artifacts/       failure-*.png, diff-*.png

asciinema v2 header line: {"version":2,"width":C,"height":R,"timestamp":epoch,"env":{"TERM":...}}, then [float_seconds, "o", "string"] per event. Recorder API: on_output(bytes), on_input(bytes), on_frame(screen, step), begin_step/end_step, on_assertion, flush(). The recorder subscribes to the actor's event broadcast and must not lag (give it a dedicated buffered channel, not the lossy broadcast). Acceptance: output.cast replays in a stock asciinema player; pros trace view reconstructs every frame.
14. Actor system (pros-engine-actor)

Hierarchy: SessionManager → Session → Context → Terminal(actor). Each Terminal is a tokio task owning Pty, Box<dyn Emulator>, Synchronizer, Recorder. Communication:
rust

enum TermCmd {
    Write(Bytes), Key(KeyEvent), Mouse(MouseEvent), Resize(u16,u16), Paste(Bytes),
    Resolve { loc: Locator, multiline: bool, deadline: Instant, tx: oneshot::Sender<Vec<Match>> },
    Snapshot { kind: SnapshotKind, deadline: Instant, tx: oneshot::Sender<Snapshot> },
    BeginStep(String), EndStep, Trace(TraceCmd), SetProfile(Box<Profile>),
    Shutdown,
}
struct TerminalHandle { cmd_tx: mpsc::Sender<TermCmd>, events: broadcast::Sender<FrameEvent> }

Task loop (tokio select!):

    pty.read() → emulator.advance(bytes) → recorder.on_output → synchronizer.on_output → if Stable: build Screen, bump generation, send FrameEvent, recorder.on_frame, evaluate registered waiters.
    cmd_rx.recv() → encode/apply (write→pty; key→encode_key→pty; resize→pty.resize + emulator.resize; resolve/snapshot→register waiter with deadline). After applying, arm the synchronizer.
    After each advance, emulator.drain_replies() → pty.write (answer DA/DSR) → recorder.on_input.
    On Shutdown / EOF: pty.kill, drain pending waiters with Err(Closed), recorder.flush.

Backpressure: pty read is the producer; the loop processes inline (cheap). Waiters resolved on FrameEvent or safety tick. Supervision: a panicking actor closes its handle; Context surfaces it as EngineError::TerminalCrashed. Acceptance: 100 concurrent contexts each spawning cat and writing/expecting echo complete in < 2 s on CI.
15. Protocol + engine server (proto/, pros-proto, pros-engine)

Single source of truth proscenium.proto (abridged — agent fills field numbers contiguously):
proto

syntax = "proto3"; package proscenium.v1;

service Proscenium {
  rpc Handshake(HandshakeReq) returns (HandshakeResp);     // negotiate protocol_version
  rpc NewContext(NewContextReq) returns (ContextId);
  rpc CloseContext(ContextId) returns (Ack);
  rpc Spawn(SpawnReq) returns (TerminalId);
  rpc Write(WriteReq) returns (Ack);
  rpc Key(KeyReq) returns (Ack);
  rpc Mouse(MouseReq) returns (Ack);
  rpc Resize(ResizeReq) returns (Ack);
  rpc Paste(PasteReq) returns (Ack);
  rpc ResolveLocator(ResolveReq) returns (ResolveResp);    // server retries to deadline_ms
  rpc Snapshot(SnapshotReq) returns (SnapshotResp);
  rpc Assert(AssertReq) returns (AssertResp);              // server-side web-first assertion
  rpc SetProfile(SetProfileReq) returns (Capabilities);
  rpc BeginStep(StepReq) returns (Ack);
  rpc EndStep(TerminalIdMsg) returns (Ack);
  rpc StartTrace(TraceReq) returns (Ack);
  rpc ExportTrace(TerminalIdMsg) returns (TracePath);
  rpc Subscribe(SubscribeReq) returns (stream Event);      // output bytes + frame deltas
}
message KeyReq { string terminal=1; Key key=2; uint32 mods=3; }
message Key { oneof k { string char=1; SpecialKey special=2; } }
enum SpecialKey { ENTER=0; TAB=1; UP=2; /* … */ }
message ResolveReq { string terminal=1; Locator loc=2; bool multiline=3; uint32 deadline_ms=4; }
message SnapshotResp { oneof s { string text=1; StyledSnapshot styled=2; bytes png=3; } }
message Locator { oneof l { TextLoc text=1; string regex=2; CellLoc cell=3; RectLoc region=4; StyledLoc styled=5; CursorLoc cursor=6; LineLoc line=7; } }
// Cell, CellStyle, Color, Capabilities, Event{ oneof { OutputChunk, Frame } } …

Server (pros-engine): tonic service holding the SessionManager. Handlers translate proto→domain (pros-proto::map), call the relevant TerminalHandle, translate the result back. ResolveLocator/Assert pass deadline_ms into the actor's waiter so retry happens server-side. Subscribe bridges the actor broadcast to a tonic server stream. Transport: Unix domain socket by default (path in config), TCP optional. Frame with Connect/gRPC (Connect for browser compatibility). Acceptance: a raw gRPC client can NewContext → Spawn(["echo","hi"]) → ResolveLocator(Text "hi", 1000ms) and get one match.
16. Codegen pipeline (buf)

buf.gen.yaml drives all SDK base generation from proto/:
yaml

version: v2
plugins:
  - { remote: buf.build/protocolbuffers/go,      out: sdks/go/gen }
  - { remote: buf.build/connectrpc/go,           out: sdks/go/gen }
  - { remote: buf.build/connectrpc/es,           out: sdks/ts/gen }      # TS, browser+node
  - { remote: buf.build/protocolbuffers/python,  out: sdks/python/gen }
  - { local: protoc-gen-grpc-python,             out: sdks/python/gen }
  - { protoc_builtin: cpp,                        out: sdks/cpp/gen }      # + grpc_cpp plugin

Rust server/client via tonic-build in pros-proto/build.rs. CI gates: buf lint, buf breaking --against '.git#branch=main'. Generated code is committed (so consumers don't need the toolchain) and regenerated by pros codegen / CI. Acceptance: buf generate produces compiling base clients in all five languages; buf breaking fails a PR that renames a field.
17. Embedded FFI (pros-ffi)

C ABI, opaque handles, explicit ownership. cbindgen emits ffi-headers/proscenium.h.
rust

#[repr(C)] pub struct ProsContext { _p: [u8;0] }
#[repr(C)] pub enum ProsStatus { Ok=0, Timeout=1, NotFound=2, Crashed=3, BadArg=4, Internal=5 }
#[no_mangle] pub extern "C" fn pros_context_new(profile: *const c_char, cols: u16, rows: u16) -> *mut ProsContext;
#[no_mangle] pub extern "C" fn pros_spawn(ctx: *mut ProsContext, argv: *const *const c_char, argc: usize) -> *mut ProsTerminal;
#[no_mangle] pub extern "C" fn pros_write(t: *mut ProsTerminal, data: *const u8, len: usize) -> ProsStatus;
#[no_mangle] pub extern "C" fn pros_resolve_text(t: *mut ProsTerminal, pat: *const c_char, deadline_ms: u32, out_found: *mut bool) -> ProsStatus;
#[no_mangle] pub extern "C" fn pros_snapshot_text(t: *mut ProsTerminal, out: *mut *mut c_char) -> ProsStatus; // caller frees via pros_string_free
#[no_mangle] pub extern "C" fn pros_string_free(s: *mut c_char);
#[no_mangle] pub extern "C" fn pros_terminal_free(t: *mut ProsTerminal);
#[no_mangle] pub extern "C" fn pros_context_free(c: *mut ProsContext);

Ownership rules: every *mut returned by the lib is freed by a matching pros_*_free; out-strings are heap-allocated by Rust and freed by pros_string_free; the lib never frees caller memory. Embedded mode runs a single-threaded tokio runtime internally (lazy-init global). Higher-level idiomatic wrappers generated by Diplomat (C++, JS/WASM, Python) and UniFFI (Python/Swift/Kotlin, Go via uniffi-bindgen-go) layered on this surface. Embedded surface is reduced (spawn/input/resolve/snapshot only); tracing/matrix/remote are daemon-only.
18. SDK sugar contract (sdks/*)

Each SDK = generated base + hand-written sugar. Sugar MUST implement this shared behavioral contract so the conformance suite passes uniformly:

    Terminal.spawn(argv, {profile, cols, rows, env}), .write/press/click/resize/paste.
    Lazy Locator objects from getByText/getByRegex/getByCell/getByRegion/getCursor.
    expect(locator).toBeVisible/toHaveText/toContainText/toHaveStyle() — each maps to a server-side Assert with default deadline_ms = 5000.
    expect(terminal).toMatchSnapshot(name, {kind, masks, normalize, tolerance}).
    test(name, fn) + matrix({profiles, shells, sizes}) expansion (or delegate expansion to the runner).
    Errors map 1:1 to the §21 taxonomy.

Default deadlines, retry cadence, snapshot path scheme, and normalization semantics are identical across SDKs (enforced by conformance). Acceptance: the same logical test, written in Rust and Go sugar, produces identical pass/fail and identical snapshot bytes against the reference engine.
19. Runner (pros-runner)

Responsibilities: discover tests, expand matrix to the cartesian product (profile × shell × size), schedule across a bounded worker pool (default = CPU count), each test in a fresh Context. Retry policy: --retries N with flake quarantine (a test that fails then passes is reported flaky, not pass). Reporters: pretty (TTY, live), junit (XML), json, github (annotations). Flags: --update-snapshots, --grep, --shard i/n, --profile, --watch. Acceptance: pros run --profile xterm,vt220 --size 80x24 runs each test twice (once per profile) in isolated contexts and emits valid JUnit.
20. Conformance / differential harness (conformance/)

The trust anchor for the multi-SDK and multi-emulator story.

Emulator corpus (emulator/*.yaml):
yaml

name: sgr_basic
profile: xterm
feed: "\e[1;31mHI\e[0m"
expect:
  cursor: {row: 0, col: 2}
  lines:
    - text: "HI"
      styles: [{start: 0, len: 2, fg: {indexed: 1}, attrs: [BOLD]}]

Runner feeds feed to a fresh emulator, asserts expect. Also a differential mode (P2): pipe feed through real xterm/kitty headless (Xvfb) via tmux capture-pane and compare to the model, flagging divergences.

Protocol corpus (protocol/*.yaml):
yaml

name: spawn_echo
steps:
  - rpc: NewContext   ; req: {profile: xterm, cols: 80, rows: 24}
  - rpc: Spawn        ; req: {argv: ["echo","hi"]}
  - rpc: ResolveLocator; req: {text: "hi", deadline_ms: 1000} ; expect: {match_count: 1}

Run against (a) the reference Rust engine, and (b) every SDK (each SDK has a tiny conformance driver that executes steps via its sugar and asserts). This is what guarantees Go ≡ C++ ≡ Python ≡ Rust. Acceptance: all corpora green against engine + all built SDKs in CI.
21. Error model

pros-core::Error (thiserror), mapped to gRPC Status codes and FFI ProsStatus:
Domain error	gRPC	FFI	Meaning
SpawnFailed	INTERNAL	Internal	PTY/exec failed
Timeout	DEADLINE_EXCEEDED	Timeout	locator/assert deadline
NotFound	NOT_FOUND	NotFound	no match / bad id
TerminalCrashed	ABORTED	Crashed	actor/SUT died
BadArgument	INVALID_ARGUMENT	BadArg	malformed request
ProtocolMismatch	FAILED_PRECONDITION	BadArg	version negotiation
Internal	INTERNAL	Internal	bug

Assertions return a structured AssertResp{ok, actual, expected, detail} rather than erroring on logical failure (only transport/engine faults are errors).
22. Configuration (proscenium.toml)
toml

[engine
] socket = "/tmp/pros.sock"  protocol_version = 1
[sync
]   quiet_window_ms = 50  max_settle_ms = 2000  tick_ms = 10
[defaults
] profile = "xterm"  cols = 80  rows = 24  assert_deadline_ms = 5000
[snapshots
] dir = "snapshots"  update = false  pixel_scale = 1
[runner
] workers = 0  retries = 0  reporter = "pretty"
[[normalize
]] re = '\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}'  replace = "<TS>"

Precedence: CLI flags > env (PROS_*) > proscenium.toml > built-in defaults. Loaded by pros-core::config, validated at startup.
23. Observability

tracing crate throughout; RUST_LOG/PROS_LOG controls level. Spans per RPC and per actor command. pros doctor prints: toolchain, font hash, available shells, profile table, socket reachability, and a self-test (spawn echo, assert). Metrics (optional, P2): contexts active, frames/s, assert latency histogram.
24. Security

The engine spawns arbitrary programs — treat it as locally trusted only. UDS default with 0600 perms; TCP requires an explicit --listen and a token (PROS_TOKEN, checked in Handshake). Never log full env (may contain secrets) — redact values, keep keys. Remote PTY (P2) requires explicit host allowlist. SUT output is never eval'd.
25. Build / CI / release

CI jobs: (1) cargo fmt --check, clippy -D warnings, cargo test (all crates); (2) buf lint + buf breaking; (3) buf generate + build each SDK + run its conformance driver; (4) cross-OS pixel-determinism test (Linux+macOS); (5) emulator corpus. Release: static engine binary per platform (musl on Linux), libpros artifacts + proscenium.h, and publish SDKs to crates.io / npm / PyPI / Go module proxy / vcpkg+Conan. Pin the embedded font; its SHA is asserted in tests so it can't drift.
