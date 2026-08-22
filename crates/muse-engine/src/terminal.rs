//! The Terminal actor (§14): owns Pty + Emulator + Synchronizer + Recorder,
//! runs as a tokio task, and serves commands with web-first deadline polling.

use crate::sync::Synchronizer;
use bytes::Bytes;
use muse_core::config::SyncConfig;
use muse_core::error::{Error, Result};
use muse_core::input::{encode_key, encode_mouse, encode_paste, KeyEvent, MouseEvent};
use muse_core::locator::{resolve, Locator, Match};
use muse_core::screen::Screen;
use muse_core::snapshot::{Snapshot, SnapshotKind};
use muse_core::Profile;
use muse_emulator::Emulator;
use muse_pty::Pty;
use muse_render::{DefaultRenderer, Renderer};
use muse_trace::{Recorder, TraceMeta};
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::Duration;

/// A frame emitted when the synchronizer declares stability.
#[derive(Clone, Debug)]
pub struct FrameEvent {
    pub generation: u64,
    pub screen: Screen,
}

pub enum TermCmd {
    Write(Bytes),
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Paste(Bytes),
    Resolve {
        loc: Locator,
        multiline: bool,
        deadline: Instant,
        tx: oneshot::Sender<Vec<Match>>,
    },
    Snapshot {
        kind: SnapshotKind,
        min_stable_frames: u8,
        deadline: Instant,
        tx: oneshot::Sender<Result<Snapshot>>,
    },
    BeginStep(String),
    EndStep,
    SetProfile(Box<Profile>),
    StartTrace(Box<(PathBuf, TraceMeta)>),
    ExportTrace(oneshot::Sender<Result<PathBuf>>),
    /// Drop the recorder without flushing (nothing will be written).
    DiscardTrace,
    /// Wait for the SUT process to exit (or until deadline).
    /// Replies with the exit code, or `None` on timeout.
    WaitExit {
        deadline: Instant,
        tx: oneshot::Sender<Option<u32>>,
    },
    /// Record an assertion verdict into the active trace step.
    RecordAssertion {
        kind: String,
        ok: bool,
        detail: String,
    },
    /// The live screen right now (no settling) plus the stable-frame generation.
    Screen(oneshot::Sender<(Screen, u64)>),
    /// Process facts: pid, exit code (if exited), whether output hit EOF.
    Info(oneshot::Sender<TerminalInfo>),
    Shutdown,
}

/// Facts about the SUT process behind a terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalInfo {
    pub pid: Option<u32>,
    pub exit_code: Option<u32>,
    pub eof: bool,
    pub generation: u64,
}

struct ResolveWaiter {
    loc: Locator,
    multiline: bool,
    deadline: Instant,
    tx: oneshot::Sender<Vec<Match>>,
}

struct SnapshotWaiter {
    kind: SnapshotKind,
    min_stable_frames: u8,
    deadline: Instant,
    tx: oneshot::Sender<Result<Snapshot>>,
}

pub struct Terminal {
    pty: Pty,
    emu: Box<dyn Emulator>,
    sync: Synchronizer,
    recorder: Option<Recorder>,
    trace_dir: Option<PathBuf>,
    cmd_rx: mpsc::Receiver<TermCmd>,
    events: broadcast::Sender<FrameEvent>,
    start: Instant,
    generation: u64,
    last_stable: Option<Screen>,
    prev_stable: Option<Screen>,
    stable_run: u8,
    resolve_waiters: Vec<ResolveWaiter>,
    snapshot_waiters: Vec<SnapshotWaiter>,
    eof: bool,
    exit_code: Option<u32>,
    exit_waiters: Vec<(Instant, oneshot::Sender<Option<u32>>)>,
    /// Set once the SUT has written anything. Until then (and until
    /// `max_settle_ms` has elapsed) an empty screen is never declared stable,
    /// so a slow-starting program can't be snapshotted blank.
    first_output_seen: bool,
}

/// How long `Shutdown` / handle-drop waits for the SUT to exit after SIGTERM
/// before escalating to SIGKILL.
const TERMINATE_GRACE: Duration = Duration::from_millis(500);

impl Terminal {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn ts(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn process_output(&mut self, bytes: &[u8]) {
        let ts = self.ts();
        self.first_output_seen = true;
        if let Some(rec) = self.recorder.as_mut() {
            rec.on_output(ts, bytes);
        }
        self.emu.advance(bytes);
        let replies = self.emu.drain_replies();
        if !replies.is_empty() {
            let _ = self.pty.write(&replies);
            if let Some(rec) = self.recorder.as_mut() {
                rec.on_input(ts, &replies);
            }
        }
        if self.emu.take_ready() > 0 {
            self.sync.note_ready();
        }
        let sync_open = self.emu.modes().sync_output;
        self.sync.note_sync_open(sync_open);
        let now = self.now_ms();
        self.sync.note_output(now);
        self.try_emit_stable(now);
    }

    fn try_emit_stable(&mut self, now_ms: u64) {
        if !self.first_output_seen && !self.eof && now_ms < self.sync.max_settle_ms() {
            // Nothing painted yet: don't settle an empty screen right after spawn.
            return;
        }
        if !self.sync.evaluate(now_ms) {
            return;
        }
        self.sync.consume_stable();
        let screen = self.emu.snapshot_screen();
        self.generation += 1;
        if self.prev_stable.as_ref() == Some(&screen) {
            self.stable_run = self.stable_run.saturating_add(1);
        } else {
            self.stable_run = 1;
        }
        self.prev_stable = Some(screen.clone());
        self.last_stable = Some(screen.clone());
        let ts = self.ts();
        let gen = self.generation;
        tracing::trace!(
            generation = gen,
            stable_run = self.stable_run,
            "stable frame"
        );
        if let Some(rec) = self.recorder.as_mut() {
            rec.on_frame(ts, gen, screen.clone());
        }
        let _ = self.events.send(FrameEvent {
            generation: self.generation,
            screen: screen.clone(),
        });
        self.fire_waiters(&screen);
    }

    /// Resolve waiters that are now satisfiable against `screen`.
    fn fire_waiters(&mut self, screen: &Screen) {
        let mut i = 0;
        while i < self.resolve_waiters.len() {
            let matches = resolve(
                screen,
                &self.resolve_waiters[i].loc,
                self.resolve_waiters[i].multiline,
            );
            if !matches.is_empty() {
                let w = self.resolve_waiters.remove(i);
                let _ = w.tx.send(matches);
            } else {
                i += 1;
            }
        }
        let mut i = 0;
        while i < self.snapshot_waiters.len() {
            if self.stable_run >= self.snapshot_waiters[i].min_stable_frames.max(1) {
                let w = self.snapshot_waiters.remove(i);
                let snap = self.render(screen, w.kind);
                let _ = w.tx.send(Ok(snap));
            } else {
                i += 1;
            }
        }
    }

    fn render(&self, screen: &Screen, kind: SnapshotKind) -> Snapshot {
        DefaultRenderer.render(screen, kind)
    }

    fn fire_exit_waiters(&mut self) {
        for (_, tx) in self.exit_waiters.drain(..) {
            let _ = tx.send(self.exit_code);
        }
    }

    /// Resolve any waiters whose deadline has passed (return best-effort result).
    ///
    /// "Best effort at the deadline" means the screen as it is *now* — not the
    /// last stable frame, which may predate output the SUT has since written.
    fn check_deadlines(&mut self, now: Instant) {
        let current = self.emu.snapshot_screen();
        let mut i = 0;
        while i < self.resolve_waiters.len() {
            if now >= self.resolve_waiters[i].deadline {
                let w = self.resolve_waiters.remove(i);
                let matches = resolve(&current, &w.loc, w.multiline);
                let _ = w.tx.send(matches);
            } else {
                i += 1;
            }
        }
        let mut i = 0;
        while i < self.snapshot_waiters.len() {
            if now >= self.snapshot_waiters[i].deadline {
                let w = self.snapshot_waiters.remove(i);
                let snap = self.render(&current, w.kind);
                let _ = w.tx.send(Ok(snap));
            } else {
                i += 1;
            }
        }
        // fire exit waiters whose deadline has passed
        let mut i = 0;
        while i < self.exit_waiters.len() {
            if now >= self.exit_waiters[i].0 {
                let (_, tx) = self.exit_waiters.remove(i);
                let _ = tx.send(self.exit_code);
            } else {
                i += 1;
            }
        }
    }

    fn handle_cmd(&mut self, cmd: TermCmd) -> bool {
        let now = self.now_ms();
        let ts = self.ts();
        match cmd {
            TermCmd::Write(b) => {
                let _ = self.pty.write(&b);
                if let Some(rec) = self.recorder.as_mut() {
                    rec.on_input(ts, &b);
                }
                self.stable_run = 0;
                self.sync.arm(now);
            }
            TermCmd::Key(ev) => {
                let bytes = encode_key(&ev, self.emu.modes(), self.emu.capabilities());
                let _ = self.pty.write(&bytes);
                if let Some(rec) = self.recorder.as_mut() {
                    rec.on_input(ts, &bytes);
                }
                self.stable_run = 0;
                self.sync.arm(now);
            }
            TermCmd::Mouse(ev) => {
                let bytes = encode_mouse(&ev, self.emu.modes());
                if !bytes.is_empty() {
                    let _ = self.pty.write(&bytes);
                    if let Some(rec) = self.recorder.as_mut() {
                        rec.on_input(ts, &bytes);
                    }
                }
                self.stable_run = 0;
                self.sync.arm(now);
            }
            TermCmd::Resize(c, r) => {
                let _ = self.pty.resize(c, r);
                self.emu.resize(c, r);
                self.stable_run = 0;
                self.sync.arm(now);
            }
            TermCmd::Paste(b) => {
                let bytes = encode_paste(&b, self.emu.modes());
                let _ = self.pty.write(&bytes);
                if let Some(rec) = self.recorder.as_mut() {
                    rec.on_input(ts, &bytes);
                }
                self.stable_run = 0;
                self.sync.arm(now);
            }
            TermCmd::Resolve {
                loc,
                multiline,
                deadline,
                tx,
            } => {
                // immediate evaluation against the last stable screen, or the
                // live screen when nothing has settled yet
                let screen = self
                    .last_stable
                    .clone()
                    .unwrap_or_else(|| self.emu.snapshot_screen());
                let matches = resolve(&screen, &loc, multiline);
                if !matches.is_empty() {
                    let _ = tx.send(matches);
                    return true;
                }
                self.resolve_waiters.push(ResolveWaiter {
                    loc,
                    multiline,
                    deadline,
                    tx,
                });
            }
            TermCmd::Snapshot {
                kind,
                min_stable_frames,
                deadline,
                tx,
            } => {
                if let Some(screen) = &self.last_stable {
                    if self.stable_run >= min_stable_frames.max(1) {
                        let snap = self.render(screen, kind);
                        let _ = tx.send(Ok(snap));
                        return true;
                    }
                }
                self.snapshot_waiters.push(SnapshotWaiter {
                    kind,
                    min_stable_frames,
                    deadline,
                    tx,
                });
            }
            TermCmd::BeginStep(name) => {
                if let Some(rec) = self.recorder.as_mut() {
                    rec.begin_step(name, ts);
                }
            }
            TermCmd::EndStep => {
                if let Some(rec) = self.recorder.as_mut() {
                    rec.end_step(ts);
                }
            }
            TermCmd::SetProfile(p) => {
                self.emu.set_profile_dyn(*p);
            }
            TermCmd::StartTrace(b) => {
                let (dir, meta) = *b;
                self.trace_dir = Some(dir.clone());
                self.recorder = Some(Recorder::new(dir, meta));
            }
            TermCmd::ExportTrace(tx) => {
                let dir = self.trace_dir.clone().unwrap_or_default();
                let res = if let Some(rec) = self.recorder.as_mut() {
                    rec.flush()
                        .map(|_| dir)
                        .map_err(|e| Error::Internal(e.to_string()))
                } else {
                    Err(Error::NotFound("no active trace".into()))
                };
                let _ = tx.send(res);
            }
            TermCmd::DiscardTrace => {
                self.recorder = None;
                self.trace_dir = None;
            }
            TermCmd::WaitExit { deadline, tx } => {
                if let Some(code) = self.exit_code {
                    let _ = tx.send(Some(code));
                } else {
                    self.exit_waiters.push((deadline, tx));
                }
            }
            TermCmd::RecordAssertion { kind, ok, detail } => {
                if let Some(rec) = self.recorder.as_mut() {
                    rec.on_assertion(kind, ok, detail);
                }
            }
            TermCmd::Screen(tx) => {
                let _ = tx.send((self.emu.snapshot_screen(), self.generation));
            }
            TermCmd::Info(tx) => {
                let _ = tx.send(TerminalInfo {
                    pid: self.pty.pid(),
                    exit_code: self.exit_code,
                    eof: self.eof,
                    generation: self.generation,
                });
            }
            TermCmd::Shutdown => {
                return false;
            }
        }
        true
    }

    /// Stop the SUT (process group) and record its exit code.
    fn terminate(&mut self) {
        if self.exit_code.is_none() {
            let st = self.pty.terminate(TERMINATE_GRACE);
            tracing::debug!(pid = ?self.pty.pid(), code = st.code, "terminated SUT");
            self.exit_code = Some(st.code);
        }
    }

    fn finalize(&mut self, fire_exit: bool) {
        // emit a final frame from whatever state we have, resolve outstanding waiters
        let screen = self.emu.snapshot_screen();
        self.generation += 1;
        self.last_stable = Some(screen.clone());
        self.stable_run = self.stable_run.saturating_add(1);
        let ts = self.ts();
        let gen = self.generation;
        if let Some(rec) = self.recorder.as_mut() {
            rec.on_frame(ts, gen, screen.clone());
        }
        let _ = self.events.send(FrameEvent {
            generation: self.generation,
            screen: screen.clone(),
        });
        // satisfy remaining waiters best-effort
        for w in self.resolve_waiters.drain(..) {
            let matches = resolve(&screen, &w.loc, w.multiline);
            let _ = w.tx.send(matches);
        }
        for w in self.snapshot_waiters.drain(..) {
            let snap = DefaultRenderer.render(&screen, w.kind);
            let _ = w.tx.send(Ok(snap));
        }
        if let Some(rec) = self.recorder.as_mut() {
            let _ = rec.flush();
        }
        // Resolve pending exit waiters. When fire_exit is false and exit_code
        // is not yet known (the PTY slave closed but the process hasn't entered
        // zombie state yet), leave the waiters for the ticker to resolve once
        // try_wait succeeds — avoids resolving them with None prematurely.
        if fire_exit || self.exit_code.is_some() {
            self.fire_exit_waiters();
        }
    }

    async fn run(mut self) {
        let tick = Duration::from_millis(self.sync.tick_ms().max(1));
        let mut ticker = tokio::time::interval(tick);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // arm once so the initial render settles into a stable frame
        self.sync.arm(self.now_ms());
        loop {
            tokio::select! {
                biased;
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(c) => {
                            if !self.handle_cmd(c) {
                                self.terminate();
                                self.finalize(true);
                                break;
                            }
                        }
                        None => {
                            // all handles dropped: nobody can observe the SUT
                            // any more, so stop it rather than leak it
                            self.terminate();
                            self.finalize(true);
                            break;
                        }
                    }
                }
                maybe = self.pty.reader().recv(), if !self.eof => {
                    match maybe {
                        Some(bytes) => self.process_output(&bytes),
                        None => {
                            self.eof = true;
                            // Non-blocking reap — almost always succeeds immediately after EOF.
                            if let Some(st) = self.pty.try_wait() {
                                self.exit_code = Some(st.code);
                            }
                            // fire_exit=false: if exit_code is unknown (rare race where the
                            // PTY slave closed before the process entered zombie state), defer
                            // exit waiters to the ticker's try_wait retry rather than resolving
                            // them with None and discarding them.
                            self.finalize(false);
                        }
                    }
                }
                _ = ticker.tick() => {
                    let now_ms = self.now_ms();
                    self.try_emit_stable(now_ms);
                    self.check_deadlines(Instant::now());
                    // Retry reaping if EOF arrived but try_wait wasn't ready yet.
                    if self.eof && self.exit_code.is_none() {
                        if let Some(st) = self.pty.try_wait() {
                            self.exit_code = Some(st.code);
                            self.fire_exit_waiters();
                        }
                    }
                }
            }
        }
    }
}

/// Handle to a running [`Terminal`] actor.
#[derive(Clone)]
pub struct TerminalHandle {
    cmd_tx: mpsc::Sender<TermCmd>,
    events: broadcast::Sender<FrameEvent>,
}

impl TerminalHandle {
    /// Spawn the actor task and return a handle.
    pub fn spawn(pty: Pty, emu: Box<dyn Emulator>, sync_cfg: SyncConfig) -> TerminalHandle {
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let (events, _) = broadcast::channel(256);
        let term = Terminal {
            pty,
            emu,
            sync: Synchronizer::new(sync_cfg),
            recorder: None,
            trace_dir: None,
            cmd_rx,
            events: events.clone(),
            start: Instant::now(),
            generation: 0,
            last_stable: None,
            prev_stable: None,
            stable_run: 0,
            resolve_waiters: Vec::new(),
            snapshot_waiters: Vec::new(),
            eof: false,
            exit_code: None,
            exit_waiters: Vec::new(),
            first_output_seen: false,
        };
        tokio::spawn(term.run());
        TerminalHandle { cmd_tx, events }
    }

    async fn send(&self, cmd: TermCmd) -> Result<()> {
        self.cmd_tx
            .send(cmd)
            .await
            .map_err(|_| Error::TerminalCrashed("actor channel closed".into()))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<FrameEvent> {
        self.events.subscribe()
    }

    pub async fn write(&self, bytes: impl Into<Bytes>) -> Result<()> {
        self.send(TermCmd::Write(bytes.into())).await
    }

    pub async fn key(&self, ev: KeyEvent) -> Result<()> {
        self.send(TermCmd::Key(ev)).await
    }

    pub async fn mouse(&self, ev: MouseEvent) -> Result<()> {
        self.send(TermCmd::Mouse(ev)).await
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.send(TermCmd::Resize(cols, rows)).await
    }

    pub async fn paste(&self, bytes: impl Into<Bytes>) -> Result<()> {
        self.send(TermCmd::Paste(bytes.into())).await
    }

    pub async fn begin_step(&self, name: impl Into<String>) -> Result<()> {
        self.send(TermCmd::BeginStep(name.into())).await
    }

    pub async fn end_step(&self) -> Result<()> {
        self.send(TermCmd::EndStep).await
    }

    /// Record an assertion verdict into the trace (no-op without a trace).
    pub async fn record_assertion(
        &self,
        kind: impl Into<String>,
        ok: bool,
        detail: impl Into<String>,
    ) -> Result<()> {
        self.send(TermCmd::RecordAssertion {
            kind: kind.into(),
            ok,
            detail: detail.into(),
        })
        .await
    }

    pub async fn set_profile(&self, profile: Profile) -> Result<()> {
        self.send(TermCmd::SetProfile(Box::new(profile))).await
    }

    pub async fn start_trace(&self, dir: PathBuf, meta: TraceMeta) -> Result<()> {
        self.send(TermCmd::StartTrace(Box::new((dir, meta)))).await
    }

    /// Forget the trace without writing it (a passing case that keeps
    /// nothing). Must precede `shutdown`, whose finalize would flush it.
    pub async fn discard_trace(&self) -> Result<()> {
        self.send(TermCmd::DiscardTrace).await
    }

    pub async fn export_trace(&self) -> Result<PathBuf> {
        let (tx, rx) = oneshot::channel();
        self.send(TermCmd::ExportTrace(tx)).await?;
        rx.await
            .map_err(|_| Error::TerminalCrashed("actor closed".into()))?
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.send(TermCmd::Shutdown).await
    }

    /// The live screen right now, without waiting for stability, plus the
    /// current stable-frame generation.
    pub async fn screen(&self) -> Result<(Screen, u64)> {
        let (tx, rx) = oneshot::channel();
        self.send(TermCmd::Screen(tx)).await?;
        rx.await
            .map_err(|_| Error::TerminalCrashed("actor closed".into()))
    }

    /// Process facts: pid, exit code, EOF.
    pub async fn info(&self) -> Result<TerminalInfo> {
        let (tx, rx) = oneshot::channel();
        self.send(TermCmd::Info(tx)).await?;
        rx.await
            .map_err(|_| Error::TerminalCrashed("actor closed".into()))
    }

    /// Wait for the SUT process to exit naturally. Returns the exit code, or
    /// `None` if `timeout_ms` elapses before the process exits.
    pub async fn wait_exit(&self, timeout_ms: u64) -> Result<Option<u32>> {
        let (tx, rx) = oneshot::channel();
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        self.send(TermCmd::WaitExit { deadline, tx }).await?;
        rx.await
            .map_err(|_| Error::TerminalCrashed("actor closed".into()))
    }

    /// Resolve a locator, retrying server-side until `deadline_ms`.
    pub async fn resolve(
        &self,
        loc: Locator,
        multiline: bool,
        deadline_ms: u64,
    ) -> Result<Vec<Match>> {
        let (tx, rx) = oneshot::channel();
        let deadline = Instant::now() + Duration::from_millis(deadline_ms);
        self.send(TermCmd::Resolve {
            loc,
            multiline,
            deadline,
            tx,
        })
        .await?;
        rx.await
            .map_err(|_| Error::TerminalCrashed("actor closed".into()))
    }

    /// Take a snapshot once the screen is stable (or at deadline).
    pub async fn snapshot(
        &self,
        kind: SnapshotKind,
        min_stable_frames: u8,
        deadline_ms: u64,
    ) -> Result<Snapshot> {
        let (tx, rx) = oneshot::channel();
        let deadline = Instant::now() + Duration::from_millis(deadline_ms);
        self.send(TermCmd::Snapshot {
            kind,
            min_stable_frames,
            deadline,
            tx,
        })
        .await?;
        rx.await
            .map_err(|_| Error::TerminalCrashed("actor closed".into()))?
    }
}
