//! Trace recorder + reader (§13). A trace is a directory.

use crate::asciinema::Cast;
use crate::format::{AssertionRecord, FrameRecord, StepRecord, TraceMeta};
use muse_core::screen::Screen;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Records a session into a trace directory.
pub struct Recorder {
    dir: PathBuf,
    meta: TraceMeta,
    out_cast: Cast,
    in_cast: Cast,
    frames: Vec<FrameRecord>,
    steps: Vec<StepRecord>,
    cur_step: Option<StepRecord>,
    next_step_id: u64,
}

impl Recorder {
    pub fn new(dir: impl Into<PathBuf>, meta: TraceMeta) -> Recorder {
        let out_cast = Cast::new(meta.cols, meta.rows, meta.started_at, term_of(&meta));
        let in_cast = Cast::new(meta.cols, meta.rows, meta.started_at, term_of(&meta));
        Recorder {
            dir: dir.into(),
            meta,
            out_cast,
            in_cast,
            frames: Vec::new(),
            steps: Vec::new(),
            cur_step: None,
            next_step_id: 1,
        }
    }

    pub fn on_output(&mut self, ts: f64, bytes: &[u8]) {
        self.out_cast.event(ts, "o", bytes);
    }

    pub fn on_input(&mut self, ts: f64, bytes: &[u8]) {
        self.in_cast.event(ts, "i", bytes);
    }

    /// Begin a named step; returns its id.
    pub fn begin_step(&mut self, name: impl Into<String>, t0: f64) -> u64 {
        // close any open step implicitly
        if let Some(s) = self.cur_step.take() {
            self.steps.push(s);
        }
        let id = self.next_step_id;
        self.next_step_id += 1;
        self.cur_step = Some(StepRecord {
            step_id: id,
            name: name.into(),
            t0,
            t1: t0,
            assertions: Vec::new(),
        });
        id
    }

    pub fn end_step(&mut self, t1: f64) {
        if let Some(mut s) = self.cur_step.take() {
            s.t1 = t1;
            self.steps.push(s);
        }
    }

    pub fn on_assertion(&mut self, kind: impl Into<String>, ok: bool, detail: impl Into<String>) {
        let rec = AssertionRecord {
            kind: kind.into(),
            ok,
            detail: detail.into(),
        };
        if let Some(s) = self.cur_step.as_mut() {
            s.assertions.push(rec);
        } else {
            // assertion outside a step → implicit step 0
            self.steps.push(StepRecord {
                step_id: 0,
                name: "<implicit>".into(),
                t0: 0.0,
                t1: 0.0,
                assertions: vec![rec],
            });
        }
    }

    pub fn on_frame(&mut self, ts: f64, gen: u64, screen: Screen) {
        let step_id = self.cur_step.as_ref().map(|s| s.step_id).unwrap_or(0);
        self.frames.push(FrameRecord {
            ts,
            gen,
            step_id,
            screen,
        });
    }

    pub fn current_step_id(&self) -> u64 {
        self.cur_step.as_ref().map(|s| s.step_id).unwrap_or(0)
    }

    /// Write all artifacts to disk.
    pub fn flush(&mut self) -> std::io::Result<()> {
        if let Some(s) = self.cur_step.take() {
            self.steps.push(s);
        }
        std::fs::create_dir_all(&self.dir)?;
        std::fs::create_dir_all(self.dir.join("artifacts"))?;
        std::fs::write(
            self.dir.join("meta.json"),
            serde_json::to_string_pretty(&self.meta)?,
        )?;
        std::fs::write(self.dir.join("output.cast"), self.out_cast.to_text())?;
        std::fs::write(self.dir.join("input.cast"), self.in_cast.to_text())?;
        let mut frames = std::fs::File::create(self.dir.join("frames.jsonl"))?;
        for f in &self.frames {
            writeln!(frames, "{}", serde_json::to_string(f)?)?;
        }
        let mut steps = std::fs::File::create(self.dir.join("steps.jsonl"))?;
        for s in &self.steps {
            writeln!(steps, "{}", serde_json::to_string(s)?)?;
        }
        Ok(())
    }

    pub fn artifact_path(&self, name: &str) -> PathBuf {
        self.dir.join("artifacts").join(name)
    }
}

fn term_of(meta: &TraceMeta) -> String {
    meta.env
        .iter()
        .find(|(k, _)| k == "TERM")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "xterm-256color".to_string())
}

/// A loaded trace, for the viewer / `muse trace view`.
pub struct Trace {
    pub meta: TraceMeta,
    pub frames: Vec<FrameRecord>,
    pub steps: Vec<StepRecord>,
}

impl Trace {
    pub fn load(dir: impl AsRef<Path>) -> std::io::Result<Trace> {
        let dir = dir.as_ref();
        let meta: TraceMeta =
            serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json"))?)?;
        let frames = read_jsonl(&dir.join("frames.jsonl"))?;
        let steps = read_jsonl(&dir.join("steps.jsonl"))?;
        Ok(Trace {
            meta,
            frames,
            steps,
        })
    }

    /// The frame active at time `ts` (last frame with frame.ts <= ts).
    pub fn frame_at(&self, ts: f64) -> Option<&FrameRecord> {
        self.frames.iter().filter(|f| f.ts <= ts).next_back()
    }
}

fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> std::io::Result<Vec<T>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> TraceMeta {
        TraceMeta {
            version: 1,
            profile: "xterm".into(),
            cols: 80,
            rows: 24,
            env: vec![("TERM".into(), "xterm-256color".into())],
            started_at: 1700000000,
            sut_argv: vec!["echo".into()],
        }
    }

    #[test]
    fn records_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        let mut rec = Recorder::new(&path, meta());
        rec.on_output(0.0, b"hello");
        rec.on_input(0.1, b"\r");
        let id = rec.begin_step("step1", 0.0);
        assert_eq!(id, 1);
        rec.on_frame(0.2, 1, Screen::new(2, 2));
        rec.on_assertion("toBeVisible", true, "ok");
        rec.end_step(0.3);
        rec.flush().unwrap();

        let t = Trace::load(&path).unwrap();
        assert_eq!(t.meta.profile, "xterm");
        assert_eq!(t.frames.len(), 1);
        assert_eq!(t.steps.len(), 1);
        assert_eq!(t.steps[0].assertions.len(), 1);
        assert!(t.steps[0].assertions[0].ok);
    }

    #[test]
    fn output_cast_replays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t");
        let mut rec = Recorder::new(&path, meta());
        rec.on_output(0.0, b"\x1b[31mhi");
        rec.flush().unwrap();
        let cast = std::fs::read_to_string(path.join("output.cast")).unwrap();
        let (h, evs) = crate::asciinema::parse(&cast).unwrap();
        assert_eq!(h["version"], 2);
        assert_eq!(evs.len(), 1);
    }

    #[test]
    fn frame_at_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t");
        let mut rec = Recorder::new(&path, meta());
        rec.on_frame(0.0, 1, Screen::new(1, 1));
        rec.on_frame(1.0, 2, Screen::new(1, 1));
        rec.flush().unwrap();
        let t = Trace::load(&path).unwrap();
        assert_eq!(t.frame_at(0.5).unwrap().gen, 1);
        assert_eq!(t.frame_at(2.0).unwrap().gen, 2);
        assert!(t.frame_at(-1.0).is_none());
    }

    #[test]
    fn implicit_step_for_orphan_assertion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t");
        let mut rec = Recorder::new(&path, meta());
        rec.on_assertion("k", false, "d");
        rec.flush().unwrap();
        let t = Trace::load(&path).unwrap();
        assert_eq!(t.steps.len(), 1);
        assert_eq!(t.steps[0].step_id, 0);
    }

    #[test]
    fn begin_step_closes_previous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t");
        let mut rec = Recorder::new(&path, meta());
        rec.begin_step("a", 0.0);
        rec.begin_step("b", 1.0);
        rec.flush().unwrap();
        let t = Trace::load(&path).unwrap();
        assert_eq!(t.steps.len(), 2);
    }

    #[test]
    fn current_step_id_tracks() {
        let mut rec = Recorder::new("/tmp/unused-muse", meta());
        assert_eq!(rec.current_step_id(), 0);
        rec.begin_step("x", 0.0);
        assert_eq!(rec.current_step_id(), 1);
    }

    #[test]
    fn artifact_path_under_artifacts() {
        let rec = Recorder::new("/tmp/tr", meta());
        assert!(rec
            .artifact_path("failure-1.png")
            .ends_with("artifacts/failure-1.png"));
    }

    #[test]
    fn load_missing_jsonl_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("meta.json"),
            serde_json::to_string(&meta()).unwrap(),
        )
        .unwrap();
        let t = Trace::load(&path).unwrap();
        assert!(t.frames.is_empty());
    }
}
