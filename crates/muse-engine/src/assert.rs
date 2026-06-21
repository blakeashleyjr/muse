//! Web-first assertions (§18 contract, §21 structured results).
//!
//! Assertions return a structured outcome rather than erroring on logical
//! failure; only transport/engine faults are `Err`.

use crate::terminal::TerminalHandle;
use muse_core::error::Result;
use muse_core::locator::{Locator, Match, StylePredicate};
use std::time::Instant;
use tokio::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct AssertOutcome {
    pub ok: bool,
    pub actual: String,
    pub expected: String,
    pub detail: String,
}

impl AssertOutcome {
    fn ok(actual: impl Into<String>, expected: impl Into<String>) -> Self {
        AssertOutcome {
            ok: true,
            actual: actual.into(),
            expected: expected.into(),
            detail: String::new(),
        }
    }
    fn fail(
        actual: impl Into<String>,
        expected: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        AssertOutcome {
            ok: false,
            actual: actual.into(),
            expected: expected.into(),
            detail: detail.into(),
        }
    }
}

fn joined(matches: &[Match]) -> String {
    matches
        .iter()
        .map(|m| m.text.trim().to_string())
        .collect::<Vec<_>>()
        .join("")
}

/// Poll a condition over re-resolved matches until it holds or the deadline.
async fn poll_until(
    handle: &TerminalHandle,
    loc: Locator,
    multiline: bool,
    deadline_ms: u64,
    mut cond: impl FnMut(&[Match]) -> bool,
) -> Result<(bool, Vec<Match>)> {
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    let step = 75u64;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let slice = (remaining.as_millis() as u64).min(step);
        let matches = handle.resolve(loc.clone(), multiline, slice).await?;
        if cond(&matches) {
            return Ok((true, matches));
        }
        if Instant::now() >= deadline {
            return Ok((false, matches));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub async fn to_be_visible(
    handle: &TerminalHandle,
    loc: Locator,
    multiline: bool,
    deadline_ms: u64,
) -> Result<AssertOutcome> {
    let (ok, matches) = poll_until(handle, loc, multiline, deadline_ms, |m| !m.is_empty()).await?;
    Ok(if ok {
        AssertOutcome::ok(format!("{} match(es)", matches.len()), "visible")
    } else {
        AssertOutcome::fail("0 matches", "visible", "no match before deadline")
    })
}

pub async fn to_have_text(
    handle: &TerminalHandle,
    loc: Locator,
    expected: &str,
    multiline: bool,
    deadline_ms: u64,
) -> Result<AssertOutcome> {
    let exp = expected.trim().to_string();
    let (ok, matches) = poll_until(handle, loc, multiline, deadline_ms, |m| {
        !m.is_empty() && joined(m) == exp
    })
    .await?;
    let actual = joined(&matches);
    Ok(if ok {
        AssertOutcome::ok(actual, exp)
    } else {
        AssertOutcome::fail(actual, exp, "text mismatch before deadline")
    })
}

pub async fn to_contain_text(
    handle: &TerminalHandle,
    loc: Locator,
    expected: &str,
    multiline: bool,
    deadline_ms: u64,
) -> Result<AssertOutcome> {
    let exp = expected.to_string();
    let (ok, matches) = poll_until(handle, loc, multiline, deadline_ms, |m| {
        m.iter().any(|x| x.text.contains(&exp))
    })
    .await?;
    Ok(if ok {
        AssertOutcome::ok(joined(&matches), exp)
    } else {
        AssertOutcome::fail(joined(&matches), exp, "substring not found before deadline")
    })
}

pub async fn to_not_be_visible(
    handle: &TerminalHandle,
    loc: Locator,
    multiline: bool,
    deadline_ms: u64,
) -> Result<AssertOutcome> {
    let (ok, matches) = poll_until(handle, loc, multiline, deadline_ms, |m| m.is_empty()).await?;
    Ok(if ok {
        AssertOutcome::ok("0 matches", "not visible")
    } else {
        let texts: Vec<_> = matches
            .iter()
            .map(|m| format!("{:?}", m.text.trim()))
            .collect();
        AssertOutcome::fail(
            format!("{} match(es): {}", matches.len(), texts.join(", ")),
            "not visible",
            "locator still matched before deadline",
        )
    })
}

/// Assert the match count satisfies `eq`, or falls within `[min, max]`.
pub async fn to_have_count(
    handle: &TerminalHandle,
    loc: Locator,
    eq: Option<usize>,
    min: Option<usize>,
    max: Option<usize>,
    multiline: bool,
    deadline_ms: u64,
) -> Result<AssertOutcome> {
    let (ok, matches) = poll_until(handle, loc, multiline, deadline_ms, |m| {
        let n = m.len();
        eq.is_none_or(|e| n == e) && min.is_none_or(|lo| n >= lo) && max.is_none_or(|hi| n <= hi)
    })
    .await?;
    let count = matches.len();
    let constraint = format!("eq={eq:?} min={min:?} max={max:?}");
    Ok(if ok {
        AssertOutcome::ok(count.to_string(), constraint)
    } else {
        AssertOutcome::fail(
            count.to_string(),
            constraint,
            format!("count={count} did not meet constraint before deadline"),
        )
    })
}

pub async fn to_have_style(
    handle: &TerminalHandle,
    loc: Locator,
    pred: StylePredicate,
    multiline: bool,
    deadline_ms: u64,
) -> Result<AssertOutcome> {
    let (ok, _matches) = poll_until(handle, loc, multiline, deadline_ms, |m| {
        m.iter()
            .any(|x| x.styles.iter().any(|(_, st)| pred.matches(st)))
    })
    .await?;
    Ok(if ok {
        AssertOutcome::ok("style present", format!("{pred:?}"))
    } else {
        AssertOutcome::fail(
            "style absent",
            format!("{pred:?}"),
            "style not found before deadline",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn_terminal;
    use muse_core::color::Color;
    use muse_core::config::SyncConfig;
    use muse_emulator::profile;
    use std::collections::HashMap;

    fn term(argv: &[&str]) -> TerminalHandle {
        spawn_terminal(
            profile::xterm(),
            40,
            10,
            argv.iter().map(|s| s.to_string()).collect(),
            HashMap::new(),
            None,
            SyncConfig::default(),
        )
        .unwrap()
    }

    fn text(p: &str) -> Locator {
        Locator::Text {
            pattern: p.into(),
            ignore_case: false,
            whole_line: false,
        }
    }

    #[tokio::test]
    async fn have_text_mismatch_fails() {
        let h = term(&["echo", "actual"]);
        let o = to_have_text(&h, Locator::Line { row: 0 }, "expected", false, 400)
            .await
            .unwrap();
        assert!(!o.ok);
        assert!(o.detail.contains("mismatch"));
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn contain_text_mismatch_fails() {
        let h = term(&["echo", "abc"]);
        let o = to_contain_text(&h, text("a"), "ZZZ", false, 400)
            .await
            .unwrap();
        assert!(!o.ok);
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn contain_text_succeeds() {
        let h = term(&["echo", "hello-world"]);
        let o = to_contain_text(&h, Locator::Line { row: 0 }, "world", false, 2000)
            .await
            .unwrap();
        assert!(o.ok, "{o:?}");
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn have_style_detects_color() {
        // printf a red word
        let h = term(&["sh", "-c", "printf '\\033[31mRED\\033[0m\\n'"]);
        let pred = StylePredicate {
            fg: Some(Color::Indexed(1)),
            ..Default::default()
        };
        let o = to_have_style(&h, text("RED"), pred, false, 2000)
            .await
            .unwrap();
        assert!(o.ok, "{o:?}");
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn have_style_absent_fails() {
        let h = term(&["echo", "plain"]);
        let pred = StylePredicate {
            fg: Some(Color::Indexed(9)),
            ..Default::default()
        };
        let o = to_have_style(&h, text("plain"), pred, false, 400)
            .await
            .unwrap();
        assert!(!o.ok);
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn have_text_succeeds() {
        let h = term(&["echo", "exactmatch"]);
        let o = to_have_text(&h, Locator::Line { row: 0 }, "exactmatch", false, 2000)
            .await
            .unwrap();
        assert!(o.ok, "{o:?}");
        h.shutdown().await.ok();
    }

    #[tokio::test]
    async fn errors_propagate_after_shutdown() {
        let h = term(&["echo", "x"]);
        h.shutdown().await.ok();
        // give the actor a moment to exit
        tokio::time::sleep(Duration::from_millis(100)).await;
        let r = to_be_visible(&h, text("x"), false, 200).await;
        assert!(r.is_err());
    }
}
