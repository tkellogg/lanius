//! Queryable projection for coding-session observations.
//!
//! Coding agents publish their durable telemetry as flight-recorder lines in
//! `trace.jsonl`, not as ledger events. This module keeps sqlite as a derived
//! index over that append-only log: `project_trace` resumes from a byte cursor,
//! reads only complete JSONL records, and folds coding-session `obs/agent/...`
//! observations into compact session stats plus a small per-session timeline.
//! The trace remains the source of truth; these tables are deliberately scoped
//! to the web/API queries that need fast answers without replaying the log.

use crate::paths::Root;
use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// Create the projection tables and cursor row if they do not already exist.
///
/// The schema is owned here rather than in `db.rs` because the projection is a
/// derived, lazily-maintained index over the recorder's `trace.jsonl`. The stats
/// table has one row per elanus coding session for list/tree queries; the events
/// table stores only short timeline entries so UI details do not need to scan
/// the full flight recorder; the cursor table stores the byte offset of the last
/// fully-consumed JSONL line.
fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS code_session_stats (
  elanus_session TEXT PRIMARY KEY,
  tool TEXT,
  agent_noun TEXT,
  native_session TEXT,
  workdir TEXT,
  model TEXT,
  effort TEXT,
  parent TEXT,
  started_at TEXT,
  ended_at TEXT,
  exit_code INTEGER,
  last_status TEXT,
  resume_count INTEGER NOT NULL DEFAULT 0,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT
);

CREATE TABLE IF NOT EXISTS code_session_events (
  id INTEGER PRIMARY KEY,
  elanus_session TEXT NOT NULL,
  ts TEXT,
  kind TEXT,
  summary TEXT,
  created_at TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_code_session_events_session ON code_session_events(elanus_session);

CREATE TABLE IF NOT EXISTS code_projection_cursor (
  id INTEGER PRIMARY KEY CHECK (id=0),
  trace_offset INTEGER NOT NULL
);
"#,
    )
}

fn parsed_topic(topic: &str) -> Option<(&str, &str, &str)> {
    let rest = topic.strip_prefix("obs/agent/")?;
    let mut parts = rest.splitn(3, '/');
    let noun = parts.next()?;
    if noun != "codex" && noun != "claude-code" {
        return None;
    }
    let session = parts.next()?;
    if !session.starts_with("code-") {
        return None;
    }
    let leaf = parts.next()?;
    if leaf.is_empty() {
        return None;
    }
    Some((noun, session, leaf))
}

fn text_field(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_string)
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn int_field(payload: &Value, key: &str) -> Option<i64> {
    payload
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok())))
}

fn usage_field(payload: &Value, key: &str) -> i64 {
    payload
        .get("usage")
        .and_then(|usage| usage.get(key))
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok())))
        .unwrap_or(0)
}

fn clip_ascii(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[{} bytes total]", &s[..end], s.len())
}

fn compact_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        _ => value.to_string(),
    }
}

fn summary(payload: &Value) -> Option<String> {
    for key in [
        "text",
        "result",
        "command",
        "input",
        "arguments",
        "content",
        "output",
        "error",
        "changes",
        "query",
        "tool",
        "event",
        "status",
    ] {
        if let Some(value) = payload.get(key) {
            if !value.is_null() {
                return Some(clip_ascii(&compact_value(value), 200));
            }
        }
    }
    if payload.is_null() {
        None
    } else {
        Some(clip_ascii(&payload.to_string(), 200))
    }
}

fn touch_session(
    conn: &Connection,
    session: &str,
    noun: &str,
    updated_at: String,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO code_session_stats (elanus_session, agent_noun, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(elanus_session) DO UPDATE SET
           agent_noun=COALESCE(code_session_stats.agent_noun, excluded.agent_noun),
           updated_at=excluded.updated_at",
        params![session, noun, updated_at],
    )?;
    Ok(())
}

fn apply_event(conn: &Connection, topic: &str, payload: &Value) -> rusqlite::Result<()> {
    let Some((noun, session, leaf)) = parsed_topic(topic) else {
        return Ok(());
    };
    let ts = text_field(payload, "ts");
    let updated_at = ts.clone().unwrap_or_else(now_iso);
    match leaf {
        "session/start" => {
            conn.execute(
                "INSERT INTO code_session_stats
                 (elanus_session, tool, agent_noun, workdir, model, effort, parent, started_at, last_status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running', ?9)
                 ON CONFLICT(elanus_session) DO UPDATE SET
                   tool=COALESCE(excluded.tool, code_session_stats.tool),
                   agent_noun=excluded.agent_noun,
                   workdir=COALESCE(excluded.workdir, code_session_stats.workdir),
                   model=COALESCE(excluded.model, code_session_stats.model),
                   effort=COALESCE(excluded.effort, code_session_stats.effort),
                   parent=COALESCE(excluded.parent, code_session_stats.parent),
                   started_at=COALESCE(excluded.started_at, code_session_stats.started_at),
                   last_status='running',
                   updated_at=excluded.updated_at",
                params![
                    session,
                    text_field(payload, "tool"),
                    noun,
                    text_field(payload, "workdir"),
                    text_field(payload, "model"),
                    text_field(payload, "effort"),
                    text_field(payload, "parent"),
                    ts,
                    updated_at,
                ],
            )?;
        }
        "session/thread" if noun == "codex" => {
            conn.execute(
                "INSERT INTO code_session_stats (elanus_session, agent_noun, native_session, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(elanus_session) DO UPDATE SET
                   agent_noun=excluded.agent_noun,
                   native_session=COALESCE(excluded.native_session, code_session_stats.native_session),
                   updated_at=excluded.updated_at",
                params![session, noun, text_field(payload, "codex_thread"), updated_at],
            )?;
        }
        "session/started" if noun == "claude-code" => {
            conn.execute(
                "INSERT INTO code_session_stats (elanus_session, agent_noun, native_session, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(elanus_session) DO UPDATE SET
                   agent_noun=excluded.agent_noun,
                   native_session=COALESCE(excluded.native_session, code_session_stats.native_session),
                   updated_at=excluded.updated_at",
                params![session, noun, text_field(payload, "cc_session"), updated_at],
            )?;
        }
        "session/resume" => {
            conn.execute(
                "INSERT INTO code_session_stats
                 (elanus_session, agent_noun, last_status, resume_count, updated_at)
                 VALUES (?1, ?2, 'running', 1, ?3)
                 ON CONFLICT(elanus_session) DO UPDATE SET
                   agent_noun=excluded.agent_noun,
                   resume_count=code_session_stats.resume_count + 1,
                   last_status='running',
                   updated_at=excluded.updated_at",
                params![session, noun, updated_at],
            )?;
        }
        "session/idle" => {
            conn.execute(
                "INSERT INTO code_session_stats
                 (elanus_session, agent_noun, last_status, input_tokens, output_tokens, updated_at)
                 VALUES (?1, ?2, 'idle', ?3, ?4, ?5)
                 ON CONFLICT(elanus_session) DO UPDATE SET
                   agent_noun=excluded.agent_noun,
                   input_tokens=code_session_stats.input_tokens + excluded.input_tokens,
                   output_tokens=code_session_stats.output_tokens + excluded.output_tokens,
                   last_status='idle',
                   updated_at=excluded.updated_at",
                params![
                    session,
                    noun,
                    usage_field(payload, "input_tokens"),
                    usage_field(payload, "output_tokens"),
                    updated_at,
                ],
            )?;
        }
        "session/stop" => {
            conn.execute(
                "INSERT INTO code_session_stats
                 (elanus_session, agent_noun, ended_at, exit_code, last_status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'done', ?5)
                 ON CONFLICT(elanus_session) DO UPDATE SET
                   agent_noun=excluded.agent_noun,
                   ended_at=COALESCE(excluded.ended_at, code_session_stats.ended_at),
                   exit_code=COALESCE(excluded.exit_code, code_session_stats.exit_code),
                   last_status='done',
                   updated_at=excluded.updated_at",
                params![session, noun, ts, int_field(payload, "exit_code"), updated_at],
            )?;
        }
        _ => {
            touch_session(conn, session, noun, updated_at)?;
            conn.execute(
                "INSERT INTO code_session_events (elanus_session, ts, kind, summary)
                 VALUES (?1, ?2, ?3, ?4)",
                params![session, ts, leaf, summary(payload)],
            )?;
        }
    }
    Ok(())
}

fn cursor(conn: &Connection) -> rusqlite::Result<i64> {
    Ok(conn
        .query_row(
            "SELECT trace_offset FROM code_projection_cursor WHERE id=0",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

fn save_cursor(conn: &Connection, offset: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO code_projection_cursor (id, trace_offset)
         VALUES (0, ?1)
         ON CONFLICT(id) DO UPDATE SET trace_offset=excluded.trace_offset",
        params![offset],
    )?;
    Ok(())
}

/// Project new complete `trace.jsonl` observations into sqlite.
///
/// The saved cursor is a byte offset, so repeated calls are incremental and a
/// call with no new complete lines performs no data changes. If the trace file
/// has been truncated or rotated below the saved offset, the cursor resets to
/// zero and the current file is replayed from the start. Malformed JSONL records
/// and unmapped observation topics are skipped line-by-line; a bad recorder line
/// must not block the rest of the projection.
pub fn project_trace(root: &Root) -> Result<usize> {
    let mut conn = crate::db::open(root)?;
    init_schema(&conn)?;
    let path = root.trace_file();
    let mut offset = cursor(&conn)?.max(0) as u64;
    let len = match std::fs::metadata(&path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if offset != 0 {
                save_cursor(&conn, 0)?;
            }
            return Ok(0);
        }
        Err(e) => return Err(e.into()),
    };
    if len < offset {
        offset = 0;
    }

    let mut file = File::open(&path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let Some(last_newline) = bytes.iter().rposition(|b| *b == b'\n') else {
        if offset == 0 && cursor(&conn)? != 0 {
            save_cursor(&conn, 0)?;
        }
        return Ok(0);
    };
    let consumed = last_newline + 1;
    let new_offset = offset + consumed as u64;
    let complete = &bytes[..consumed];
    let tx = conn.transaction()?;
    let mut applied = 0usize;

    for line in complete.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(kind) = v.get("kind").and_then(Value::as_str) else {
            continue;
        };
        if !kind.starts_with("obs/agent/") {
            continue;
        }
        let Some(payload) = v.get("payload") else {
            continue;
        };
        let is_coding = parsed_topic(kind).is_some();
        if apply_event(&tx, kind, payload).is_ok() && is_coding {
            applied += 1;
        }
    }
    save_cursor(&tx, i64::try_from(new_offset).unwrap_or(i64::MAX))?;
    tx.commit()?;
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn temp_root(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!(
            "elanus-code-projection-{tag}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    fn append_trace(root: &Root, kind: &str, payload: Value) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.trace_file())
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({
                "ts": payload.get("ts").cloned().unwrap_or_else(|| json!("2026-06-20T00:00:00.000Z")),
                "kind": kind,
                "payload": payload,
                "sender": "test",
            })
        )
        .unwrap();
    }

    #[test]
    fn projects_codex_trace_incrementally() {
        let root = temp_root("codex");
        append_trace(
            &root,
            "obs/agent/codex/code-test123/session/start",
            json!({
                "ts": "2026-06-20T00:00:00.000Z",
                "tool": "codex",
                "workdir": "/work",
                "parent": "code-parent",
                "model": "gpt-5",
                "effort": "high"
            }),
        );
        append_trace(
            &root,
            "obs/agent/codex/code-test123/session/thread",
            json!({
                "ts": "2026-06-20T00:00:01.000Z",
                "codex_thread": "thread-1"
            }),
        );
        append_trace(
            &root,
            "obs/agent/codex/code-test123/tool/shell/call",
            json!({
                "ts": "2026-06-20T00:00:02.000Z",
                "tool": "shell",
                "command": "cargo test"
            }),
        );
        append_trace(
            &root,
            "obs/agent/codex/code-test123/session/idle",
            json!({
                "ts": "2026-06-20T00:00:03.000Z",
                "usage": { "input_tokens": 11, "output_tokens": 7 }
            }),
        );
        append_trace(
            &root,
            "obs/agent/codex/code-test123/session/stop",
            json!({
                "ts": "2026-06-20T00:00:04.000Z",
                "exit_code": 0
            }),
        );

        assert_eq!(project_trace(&root).unwrap(), 5);
        let conn = crate::db::open(&root).unwrap();
        let row: (Option<String>, Option<String>, Option<String>, i64, i64, Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT parent, model, effort, input_tokens, output_tokens, last_status, exit_code
                 FROM code_session_stats WHERE elanus_session='code-test123'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .unwrap();
        assert_eq!(row.0.as_deref(), Some("code-parent"));
        assert_eq!(row.1.as_deref(), Some("gpt-5"));
        assert_eq!(row.2.as_deref(), Some("high"));
        assert_eq!(row.3, 11);
        assert_eq!(row.4, 7);
        assert_eq!(row.5.as_deref(), Some("done"));
        assert_eq!(row.6, Some(0));
        let native: String = conn
            .query_row(
                "SELECT native_session FROM code_session_stats WHERE elanus_session='code-test123'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(native, "thread-1");
        let event: (String, String) = conn
            .query_row(
                "SELECT kind, summary FROM code_session_events WHERE elanus_session='code-test123'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(event.0, "tool/shell/call");
        assert!(event.1.contains("cargo test"));

        assert_eq!(project_trace(&root).unwrap(), 0);
        let tokens: (i64, i64, i64) = conn
            .query_row(
                "SELECT input_tokens, output_tokens, (SELECT count(*) FROM code_session_events WHERE elanus_session='code-test123')
                 FROM code_session_stats WHERE elanus_session='code-test123'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(tokens, (11, 7, 1));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn ignores_non_coding_obs_lines() {
        let root = temp_root("ignore");
        append_trace(
            &root,
            "obs/fs/path",
            json!({
                "ts": "2026-06-20T00:00:00.000Z",
                "path": "/tmp/x"
            }),
        );
        assert_eq!(project_trace(&root).unwrap(), 0);
        let conn = crate::db::open(&root).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM code_session_stats", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        std::fs::remove_dir_all(&root.dir).ok();
    }
}
