//! Animation stabilization (§12): auto-mask volatile cells, stable-frame gate.

use muse_core::grid::Rect;
use muse_core::screen::Screen;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub enum StabilizeMode {
    #[default]
    Off,
    RequireStableFrames(u8),
    AutoMaskVolatile {
        #[serde(with = "duration_ms")]
        window: Duration,
    },
}

mod duration_ms {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;
    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        (d.as_millis() as u64).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let ms = u64::deserialize(d)?;
        Ok(Duration::from_millis(ms))
    }
}

/// Compute the set of cells (as 1×1 rects) that change across the given frames.
pub fn auto_mask_volatile(frames: &[Screen]) -> Vec<Rect> {
    let mut volatile = Vec::new();
    if frames.len() < 2 {
        return volatile;
    }
    let (rows, cols) = frames[0].active_grid().dims();
    for r in 0..rows {
        for c in 0..cols {
            let first = frames[0].active_grid().cell(r, c);
            let changed = frames[1..]
                .iter()
                .any(|f| f.active_grid().cell(r, c) != first);
            if changed {
                volatile.push(Rect::cell(r, c));
            }
        }
    }
    volatile
}

/// True if the last `k` frames are byte-identical (RequireStableFrames gate).
pub fn frames_stable(frames: &[Screen], k: u8) -> bool {
    let k = k.max(1) as usize;
    if frames.len() < k {
        return false;
    }
    let tail = &frames[frames.len() - k..];
    tail.windows(2).all(|w| w[0] == w[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::cell::Cell;
    use muse_core::style::CellStyle;

    fn screen_with(ch: &str) -> Screen {
        let mut s = Screen::new(1, 3);
        s.primary.set(0, 0, Cell::glyph(ch, CellStyle::default()));
        s
    }

    #[test]
    fn detects_volatile_cell() {
        let frames = vec![screen_with("a"), screen_with("b"), screen_with("c")];
        let v = auto_mask_volatile(&frames);
        assert_eq!(v, vec![Rect::cell(0, 0)]);
    }

    #[test]
    fn no_volatile_when_stable() {
        let frames = vec![screen_with("a"), screen_with("a")];
        assert!(auto_mask_volatile(&frames).is_empty());
    }

    #[test]
    fn single_frame_no_volatile() {
        assert!(auto_mask_volatile(&[screen_with("a")]).is_empty());
    }

    #[test]
    fn frames_stable_true() {
        let frames = vec![
            screen_with("a"),
            screen_with("b"),
            screen_with("b"),
            screen_with("b"),
        ];
        assert!(frames_stable(&frames, 3));
    }

    #[test]
    fn frames_stable_false() {
        let frames = vec![screen_with("a"), screen_with("b"), screen_with("c")];
        assert!(!frames_stable(&frames, 3));
    }

    #[test]
    fn frames_stable_insufficient() {
        assert!(!frames_stable(&[screen_with("a")], 3));
    }

    #[test]
    fn mode_serde() {
        let m = StabilizeMode::AutoMaskVolatile {
            window: Duration::from_millis(250),
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("250"));
        let back: StabilizeMode = serde_json::from_str(&s).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn mode_default_off() {
        assert_eq!(StabilizeMode::default(), StabilizeMode::Off);
    }
}
