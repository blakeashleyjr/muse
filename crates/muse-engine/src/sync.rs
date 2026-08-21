//! The synchronizer (§8): deterministic "render complete" detection.
//!
//! Time is expressed in milliseconds (monotonic) so the state machine is pure
//! and fully unit-testable; the actor supplies elapsed-ms from an `Instant`.

use muse_core::config::SyncConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    Idle,
    Armed,
    Receiving,
    Stable,
}

pub struct Synchronizer {
    cfg: SyncConfig,
    state: SyncState,
    last_activity_ms: u64,
    armed_at_ms: u64,
    sync_open: bool,
    pending_ready: bool,
}

impl Synchronizer {
    pub fn new(cfg: SyncConfig) -> Synchronizer {
        Synchronizer {
            cfg,
            state: SyncState::Idle,
            last_activity_ms: 0,
            armed_at_ms: 0,
            sync_open: false,
            pending_ready: false,
        }
    }

    pub fn state(&self) -> SyncState {
        self.state
    }

    /// An action was issued; begin waiting for quiescence.
    pub fn arm(&mut self, now_ms: u64) {
        self.state = SyncState::Armed;
        self.armed_at_ms = now_ms;
        self.last_activity_ms = now_ms;
        self.pending_ready = false;
    }

    /// Record an output chunk.
    ///
    /// Spontaneous output while `Idle` (the SUT repainting with no input from
    /// us — an async load finishing, a spinner resolving) re-arms the machine
    /// so a fresh stable frame is emitted once it quiets down. Without this,
    /// retrying assertions would keep resolving against the last frame that
    /// happened to follow an input, never the live screen.
    pub fn note_output(&mut self, now_ms: u64) {
        self.last_activity_ms = now_ms;
        if self.state == SyncState::Idle {
            // max_settle is measured from the first spontaneous byte
            self.armed_at_ms = now_ms;
        }
        self.state = SyncState::Receiving;
    }

    /// Set whether a DEC-2026 synchronized-update block is open.
    pub fn note_sync_open(&mut self, open: bool) {
        self.sync_open = open;
    }

    /// A cooperative `muse:ready` marker was seen.
    pub fn note_ready(&mut self) {
        self.pending_ready = true;
    }

    /// Evaluate stability at `now_ms`. Returns true exactly when a Stable frame
    /// should be emitted (the caller then consumes it).
    pub fn evaluate(&mut self, now_ms: u64) -> bool {
        if self.state == SyncState::Idle || self.state == SyncState::Stable {
            return false;
        }
        // cooperative readiness short-circuits the timer
        if self.pending_ready {
            self.state = SyncState::Stable;
            return true;
        }
        // max settle cap forces stability even if still noisy
        if now_ms.saturating_sub(self.armed_at_ms) >= self.cfg.max_settle_ms {
            self.state = SyncState::Stable;
            return true;
        }
        if self.sync_open {
            return false;
        }
        if now_ms.saturating_sub(self.last_activity_ms) >= self.cfg.quiet_window_ms {
            self.state = SyncState::Stable;
            return true;
        }
        false
    }

    /// Consume the stable state; return to Idle awaiting the next action.
    pub fn consume_stable(&mut self) {
        self.state = SyncState::Idle;
        self.pending_ready = false;
    }

    pub fn tick_ms(&self) -> u64 {
        self.cfg.tick_ms
    }

    pub fn max_settle_ms(&self) -> u64 {
        self.cfg.max_settle_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SyncConfig {
        SyncConfig {
            quiet_window_ms: 50,
            max_settle_ms: 2000,
            tick_ms: 10,
        }
    }

    #[test]
    fn idle_never_stable() {
        let mut s = Synchronizer::new(cfg());
        assert!(!s.evaluate(1000));
        assert_eq!(s.state(), SyncState::Idle);
    }

    #[test]
    fn loading_then_done_single_stable() {
        // arm at 0, "loading" at 0, "done" at 30; quiet=50
        let mut s = Synchronizer::new(cfg());
        s.arm(0);
        s.note_output(0);
        assert!(!s.evaluate(40)); // only 40ms since last activity
        s.note_output(30);
        assert!(!s.evaluate(60)); // 30ms since last activity
        assert!(!s.evaluate(79));
        assert!(s.evaluate(80)); // 50ms since last activity (30)
        assert_eq!(s.state(), SyncState::Stable);
    }

    #[test]
    fn bsu_esu_suppresses_intermediate() {
        let mut s = Synchronizer::new(cfg());
        s.arm(0);
        s.note_output(0);
        s.note_sync_open(true);
        // even after quiet window, sync block open => not stable
        assert!(!s.evaluate(100));
        s.note_output(60);
        s.note_sync_open(false);
        assert!(!s.evaluate(100)); // only 40ms since last activity (60)
        assert!(s.evaluate(110)); // 50ms quiet
    }

    #[test]
    fn ready_marker_immediate() {
        let mut s = Synchronizer::new(cfg());
        s.arm(0);
        s.note_output(0);
        s.note_ready();
        assert!(s.evaluate(1)); // immediate, ignores quiet window
    }

    #[test]
    fn ready_overrides_sync_open() {
        let mut s = Synchronizer::new(cfg());
        s.arm(0);
        s.note_sync_open(true);
        s.note_ready();
        assert!(s.evaluate(1));
    }

    #[test]
    fn max_settle_forces_stable() {
        let mut s = Synchronizer::new(cfg());
        s.arm(0);
        // continuous noise keeps resetting last_activity, but max_settle caps it
        for t in (0..2000).step_by(10) {
            s.note_output(t);
            assert!(!s.evaluate(t));
        }
        s.note_output(2000);
        assert!(s.evaluate(2000));
    }

    #[test]
    fn consume_returns_to_idle() {
        let mut s = Synchronizer::new(cfg());
        s.arm(0);
        s.note_output(0);
        assert!(s.evaluate(50));
        s.consume_stable();
        assert_eq!(s.state(), SyncState::Idle);
        assert!(!s.evaluate(100));
    }

    #[test]
    fn note_output_idle_enters_receiving() {
        let mut s = Synchronizer::new(cfg());
        s.note_output(5);
        assert_eq!(s.state(), SyncState::Receiving);
    }

    #[test]
    fn spontaneous_output_becomes_stable() {
        // A settled machine sees output with no input in between: it must
        // produce a new stable frame after the quiet window.
        let mut s = Synchronizer::new(cfg());
        s.arm(0);
        s.note_output(0);
        assert!(s.evaluate(50));
        s.consume_stable();
        s.note_output(1000);
        assert!(!s.evaluate(1040));
        assert!(s.evaluate(1050));
    }

    #[test]
    fn spontaneous_noisy_output_capped_by_max_settle() {
        let mut s = Synchronizer::new(cfg());
        s.note_output(1000);
        let mut t = 1000;
        while t < 3000 {
            t += 10;
            s.note_output(t);
        }
        // armed_at was reset to 1000 when leaving Idle, so 2000ms later it caps
        assert!(s.evaluate(3000));
    }

    #[test]
    fn ready_marker_while_idle_is_stable_immediately() {
        let mut s = Synchronizer::new(cfg());
        s.arm(0);
        s.note_output(0);
        assert!(s.evaluate(50));
        s.consume_stable();
        // the marker rides the output stream: note_ready then note_output
        s.note_ready();
        s.note_output(500);
        assert!(s.evaluate(500));
    }

    #[test]
    fn max_settle_accessor() {
        assert_eq!(Synchronizer::new(cfg()).max_settle_ms(), 2000);
    }

    #[test]
    fn receiving_state_after_output() {
        let mut s = Synchronizer::new(cfg());
        s.arm(0);
        assert_eq!(s.state(), SyncState::Armed);
        s.note_output(5);
        assert_eq!(s.state(), SyncState::Receiving);
    }

    #[test]
    fn tick_accessor() {
        assert_eq!(Synchronizer::new(cfg()).tick_ms(), 10);
    }
}
