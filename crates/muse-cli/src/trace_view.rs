//! `muse trace view` — non-interactive scrubbable trace viewer.

use muse_core::error::{Error, Result};
use muse_render::text::render_text;
use muse_trace::Trace;
use std::path::Path;

pub fn summary(trace: &Trace) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "trace: profile={} size={}x{} frames={} steps={}\n",
        trace.meta.profile,
        trace.meta.cols,
        trace.meta.rows,
        trace.frames.len(),
        trace.steps.len()
    ));
    for step in &trace.steps {
        let passed = step.assertions.iter().filter(|a| a.ok).count();
        s.push_str(&format!(
            "  step {} {:?} [{:.2}s–{:.2}s] {}/{} assertions ok\n",
            step.step_id,
            step.name,
            step.t0,
            step.t1,
            passed,
            step.assertions.len()
        ));
    }
    s
}

/// Render a specific frame (by index) to text.
pub fn frame_text(trace: &Trace, index: usize) -> Result<String> {
    let frame = trace
        .frames
        .get(index)
        .ok_or_else(|| Error::NotFound(format!("frame {index}")))?;
    Ok(render_text(&frame.screen))
}

pub fn load(dir: &Path) -> Result<Trace> {
    Trace::load(dir).map_err(|e| Error::Internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::screen::Screen;
    use muse_trace::{Recorder, TraceMeta};

    fn make_trace(dir: &Path) {
        let mut rec = Recorder::new(
            dir,
            TraceMeta {
                version: 1,
                profile: "xterm".into(),
                cols: 10,
                rows: 3,
                env: vec![("TERM".into(), "xterm-256color".into())],
                started_at: 0,
                sut_argv: vec!["echo".into()],
            },
        );
        rec.begin_step("s1", 0.0);
        let mut screen = Screen::new(3, 10);
        screen
            .primary
            .set(0, 0, muse_core::cell::Cell::glyph("H", Default::default()));
        rec.on_frame(0.1, 1, screen);
        rec.on_assertion("toBeVisible", true, "");
        rec.end_step(0.2);
        rec.flush().unwrap();
    }

    #[test]
    fn summary_and_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tr");
        make_trace(&path);
        let t = load(&path).unwrap();
        let sum = summary(&t);
        assert!(sum.contains("frames=1"));
        assert!(sum.contains("steps=1"));
        assert!(sum.contains("1/1 assertions ok"));
        assert_eq!(frame_text(&t, 0).unwrap(), "H");
        assert!(frame_text(&t, 99).is_err());
    }

    #[test]
    fn load_missing_errs() {
        assert!(load(Path::new("/nonexistent/muse/trace")).is_err());
    }
}
