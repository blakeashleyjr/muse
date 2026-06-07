//! `muse-trace` — trace format, asciinema casts, recorder + reader (§13).

pub mod asciinema;
pub mod format;
pub mod recorder;

pub use asciinema::{Cast, Event};
pub use format::{AssertionRecord, FrameRecord, StepRecord, TraceMeta};
pub use recorder::{Recorder, Trace};
