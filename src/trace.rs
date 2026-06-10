use crate::paths::Root;
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};
use std::io::Write;

/// Identity fields threaded onto every trace line.
#[derive(Clone, Debug, Default)]
pub struct Ids {
    pub event_id: Option<i64>,
    pub cause_id: Option<i64>,
    pub correlation_id: Option<String>,
    pub session_id: Option<String>,
}

impl Ids {
    pub fn from_env() -> Self {
        Ids {
            event_id: std::env::var("HARNESS_EVENT_ID").ok().and_then(|v| v.parse().ok()),
            cause_id: std::env::var("HARNESS_CAUSE_ID").ok().and_then(|v| v.parse().ok()),
            correlation_id: std::env::var("HARNESS_CORRELATION_ID").ok().filter(|s| !s.is_empty()),
            session_id: None,
        }
    }
}

pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Publish a happening on its topic. `kind` is the topic; the recorder
/// decides persistence (docs/bus.md). Sink::None is live-only — once the bus
/// lands, such topics still fan out to in-process consumers; today they are
/// simply not written. Append-only, write-only: nothing reads trace.jsonl for
/// control flow. Each line is a single write() on an O_APPEND fd so concurrent
/// writers interleave at line granularity. Failures are deliberately swallowed:
/// the flight recorder must never take the plane down.
pub fn write(root: &Root, kind: &str, ids: &Ids, payload: Value) {
    let mut line = json!({ "ts": now_iso(), "kind": kind, "payload": payload });
    let obj = line.as_object_mut().unwrap();
    if let Some(v) = ids.event_id {
        obj.insert("event_id".into(), json!(v));
    }
    if let Some(v) = ids.cause_id {
        obj.insert("cause_id".into(), json!(v));
    }
    if let Some(v) = &ids.correlation_id {
        obj.insert("correlation_id".into(), json!(v));
    }
    if let Some(v) = &ids.session_id {
        obj.insert("session_id".into(), json!(v));
    }
    let buf = line.to_string();
    // Live first, disk second: the bus sees everything; the recorder only
    // decides persistence. Both are best-effort and independent.
    crate::bus::publish(root, kind, &buf);
    if crate::recorder::get(root).sink_for(kind) == crate::recorder::Sink::None {
        return;
    }
    append_line(root, &buf);
}

/// One O_APPEND write to trace.jsonl. Also the broker's path for recording
/// inbound external events — same line format, same swallowed failures.
pub fn append_line(root: &Root, line: &str) {
    let mut buf = line.to_string();
    buf.push('\n');
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.trace_file())
        .and_then(|mut f| f.write_all(buf.as_bytes()));
}

/// Truncate potentially huge strings before they land in a trace payload.
pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…[{} bytes total]", &s[..end], s.len())
    }
}
