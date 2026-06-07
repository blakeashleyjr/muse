//! asciinema v2 cast writer/reader (§13).

use serde_json::json;

/// An asciinema v2 cast: a header plus `[time, code, data]` events.
pub struct Cast {
    pub width: u16,
    pub height: u16,
    pub timestamp: u64,
    pub term: String,
    lines: Vec<String>,
}

impl Cast {
    pub fn new(width: u16, height: u16, timestamp: u64, term: impl Into<String>) -> Cast {
        Cast {
            width,
            height,
            timestamp,
            term: term.into(),
            lines: Vec::new(),
        }
    }

    fn header(&self) -> String {
        json!({
            "version": 2,
            "width": self.width,
            "height": self.height,
            "timestamp": self.timestamp,
            "env": {"TERM": self.term},
        })
        .to_string()
    }

    /// Append an event. `code` is "o" (output) or "i" (input).
    pub fn event(&mut self, time: f64, code: &str, data: &[u8]) {
        let s = String::from_utf8_lossy(data);
        let line = json!([time, code, s]).to_string();
        self.lines.push(line);
    }

    /// Serialize the full cast (header + events, newline-separated).
    pub fn to_text(&self) -> String {
        let mut out = self.header();
        for l in &self.lines {
            out.push('\n');
            out.push_str(l);
        }
        out.push('\n');
        out
    }

    pub fn event_count(&self) -> usize {
        self.lines.len()
    }
}

/// A parsed cast event.
#[derive(Debug, PartialEq)]
pub struct Event {
    pub time: f64,
    pub code: String,
    pub data: String,
}

/// Parse a cast string into (header_json, events).
pub fn parse(input: &str) -> Option<(serde_json::Value, Vec<Event>)> {
    let mut lines = input.lines();
    let header: serde_json::Value = serde_json::from_str(lines.next()?).ok()?;
    let mut events = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let arr = v.as_array()?;
        events.push(Event {
            time: arr.first()?.as_f64()?,
            code: arr.get(1)?.as_str()?.to_string(),
            data: arr.get(2)?.as_str()?.to_string(),
        });
    }
    Some((header, events))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_v2() {
        let c = Cast::new(80, 24, 1700000000, "xterm-256color");
        let (h, _) = parse(&c.to_text()).unwrap();
        assert_eq!(h["version"], 2);
        assert_eq!(h["width"], 80);
        assert_eq!(h["env"]["TERM"], "xterm-256color");
    }

    #[test]
    fn events_roundtrip() {
        let mut c = Cast::new(80, 24, 0, "xterm");
        c.event(0.5, "o", b"hello");
        c.event(1.0, "i", b"\r");
        assert_eq!(c.event_count(), 2);
        let (_, evs) = parse(&c.to_text()).unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].time, 0.5);
        assert_eq!(evs[0].code, "o");
        assert_eq!(evs[0].data, "hello");
        assert_eq!(evs[1].data, "\r");
    }

    #[test]
    fn non_utf8_lossy() {
        let mut c = Cast::new(10, 10, 0, "xterm");
        c.event(0.0, "o", &[0xff, 0xfe]);
        let (_, evs) = parse(&c.to_text()).unwrap();
        assert_eq!(evs.len(), 1);
    }

    #[test]
    fn parse_bad_returns_none() {
        assert!(parse("not json").is_none());
    }

    #[test]
    fn parse_malformed_event_is_none() {
        let s = "{\"version\":2,\"width\":1,\"height\":1}\n[\"not\",\"a\",\"valid\",\"event\",0]\n";
        // first element is a string, not a float → None
        assert!(parse(s).is_none());
    }

    #[test]
    fn parse_skips_blank_lines() {
        let s = "{\"version\":2,\"width\":1,\"height\":1}\n\n[0.0,\"o\",\"x\"]\n";
        let (_, evs) = parse(s).unwrap();
        assert_eq!(evs.len(), 1);
    }
}
