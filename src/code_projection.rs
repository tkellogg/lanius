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
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// Create the projection tables and cursor row if they do not already exist.
///
/// The schema is owned here rather than in `db.rs` because the projection is a
/// derived, lazily-maintained index over the recorder's `trace.jsonl`. The stats
/// table has one row per lanius coding session for list/tree queries; the events
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
    )?;
    // M1 (worker-legibility): the launch args (a JSON array, serialized to text) a
    // `session/start` obs carried. Nullable — pre-M1 rows and sessions launched
    // before this column existed have no recorded args. Migration for databases
    // created before the column existed; the "duplicate column" error is expected
    // and ignored (mirrors db.rs's ALTER-TABLE idiom).
    let _ = conn.execute("ALTER TABLE code_session_stats ADD COLUMN args TEXT", []);
    Ok(())
}

/// One row from the coding-session projection, plus fields derived for readers.
#[derive(Debug, Clone, Serialize)]
pub struct SessionStat {
    pub elanus_session: String,
    pub tool: Option<String>,
    pub agent_noun: Option<String>,
    pub native_session: Option<String>,
    pub workdir: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub parent: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub exit_code: Option<i64>,
    pub last_status: Option<String>,
    pub resume_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub updated_at: Option<String>,
    pub duration_ms: Option<i64>,

    // ── worker-legibility M1: launch identity ──────────────────────────────────
    /// The session's baseline intent (the launch task string), backfilled from
    /// `code_sessions.intent` — the durable field codesession.rs owns. Additive on
    /// the wire (absent/null when never recorded).
    #[serde(default)]
    pub intent: Option<String>,
    /// The launch args a `session/start` obs carried, serialized to JSON text.
    /// Additive on the wire (absent/null for sessions launched before this column
    /// existed, or with no args).
    #[serde(default)]
    pub args: Option<String>,
    /// Explicit detached-spawn launch edge (`code-spawn-*`) from the durable
    /// `code_sessions` row. Null for blocking nested launches / old rows.
    #[serde(default)]
    pub launched_by_event: Option<String>,

    // ── Thread-grouping fold (session-thread-grouping handoff, TG1) ───────────
    // These are ADDITIVE wire fields layered on top of the representative
    // incarnation's stat. On a raw (ungrouped) row they describe a single
    // incarnation: `incarnations` holds just this row's id, `relaunches` is 0,
    // `driven_resumes` equals this row's `resume_count`. On a grouped (thread)
    // row the representative carries the whole thread's fold (see `fold_threads`).
    /// The constituent incarnation ids folded into this thread, newest first.
    /// The first element is the representative (latest) incarnation, which is
    /// also `elanus_session` — the stable wire id the UI keys on.
    #[serde(default)]
    pub incarnations: Vec<String>,
    /// Manual `--resume` relaunches that minted a fresh lanius id but continued
    /// the same native thread: `incarnations - 1`. Reported SEPARATELY from
    /// `driven_resumes` (we do not conflate the two kinds of resume).
    #[serde(default)]
    pub relaunches: i64,
    /// Daemon-driven resumes (`resume_capture` reuses the id, emits
    /// `session/resume`): the SUM of `resume_count` across incarnations.
    #[serde(default)]
    pub driven_resumes: i64,
}

/// One compact timeline entry for a coding session detail view.
#[derive(Debug, Clone, Serialize)]
pub struct SessionEvent {
    pub id: i64,
    pub elanus_session: String,
    pub ts: Option<String>,
    pub kind: Option<String>,
    pub summary: Option<String>,
    pub created_at: Option<String>,
}

/// Detail payload for one coding session: stats, timeline, resume hint, children.
#[derive(Debug, Clone, Serialize)]
pub struct SessionDetail {
    pub session: SessionStat,
    pub events: Vec<SessionEvent>,
    /// The per-tool INTERACTIVE-resume suggestion (`lanius code <tool> --resume
    /// <native>`), or None when the tool has no clean managed passthrough (e.g.
    /// codex/opencode) — the UI then shows no resume hint. Suggestive only; resume
    /// is not an lanius verb (see `codeagent::interactive_resume_hint`).
    pub resume_command: Option<String>,
    pub children: Vec<SessionStat>,
}

fn parsed_topic(topic: &str) -> Option<(&str, &str, &str)> {
    let rest = topic.strip_prefix("obs/agent/")?;
    let mut parts = rest.splitn(3, '/');
    let noun = parts.next()?;
    if noun != "codex" && noun != "claude-code" {
        return None;
    }
    let session = parts.next()?;
    // principal-kind handoff M4 (optional) DEFERRED: switching this to the shared
    // kind-aware classifier (codesession::is_worker_session) would require
    // plumbing a `&Connection` into this pure topic parser that isn't otherwise
    // here. Per the handoff, M4 is purely cosmetic — this prefix test is already
    // gated behind the coding-noun subtree match above (noun ∈ {codex,
    // claude-code}), so it is a shape check on an already-coding topic, not an
    // authority decision. Left as the prefix test to avoid an unwarranted plumb.
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
    payload.get(key).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok()))
    })
}

fn usage_field(payload: &Value, key: &str) -> i64 {
    payload
        .get("usage")
        .and_then(|usage| usage.get(key))
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok()))
        })
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
    // SI2 (sibling-intent): fold a todo/task-list event into `code_session_tasks`
    // so a sibling's CURRENT task is queryable. This is IN ADDITION to the normal
    // stats/timeline handling below (a TodoWrite is still a timeline tool event, so
    // we deliberately do NOT consume the leaf here). Best-effort: an unrecognized
    // shape or ledger error is a clean no-op that never blocks the projection.
    let _ = fold_tasks(conn, session, leaf, payload, &updated_at);
    match leaf {
        "session/start" => {
            let args = payload
                .get("args")
                .filter(|v| !v.is_null())
                .map(|v| v.to_string());
            conn.execute(
                "INSERT INTO code_session_stats
                 (elanus_session, tool, agent_noun, workdir, model, effort, parent, started_at, last_status, updated_at, args)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running', ?9, ?10)
                 ON CONFLICT(elanus_session) DO UPDATE SET
                   tool=COALESCE(excluded.tool, code_session_stats.tool),
                   agent_noun=excluded.agent_noun,
                   workdir=COALESCE(excluded.workdir, code_session_stats.workdir),
                   model=COALESCE(excluded.model, code_session_stats.model),
                   effort=COALESCE(excluded.effort, code_session_stats.effort),
                   parent=COALESCE(excluded.parent, code_session_stats.parent),
                   started_at=COALESCE(excluded.started_at, code_session_stats.started_at),
                   last_status='running',
                   updated_at=excluded.updated_at,
                   args=COALESCE(excluded.args, code_session_stats.args)",
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
                    args,
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
                params![
                    session,
                    noun,
                    ts,
                    int_field(payload, "exit_code"),
                    updated_at
                ],
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

// ── SI2: project a session's todo/task list into `code_session_tasks` ─────────
//
// docs/handoffs/sibling-intent.md SI2. Two harnesses already put the task list on
// the bus; this fold turns it into a queryable per-session surface (latest-wins):
//   • Claude: `tool/TodoWrite/{call,result}` — the items ride `input`/`tool_input`.
//   • codex:  `assistant/todo` — an `items` array.
// CAUTION (the load-bearing gotcha): the harness clips tool payloads with
// `clip_value`, which SERIALIZES a non-string value to a JSON *string*. So in
// practice the `input`/`items` field is usually a JSON STRING, not a structured
// array — `extract_items` parses-if-string before reading it, and a value clipped
// past the limit (unparseable) is skipped rather than panicking.

/// Fold one todo/task-list obs event into `code_session_tasks`, replacing the
/// session's task set with exactly the items in this event (upsert each, then drop
/// any prior rows no longer present — a shrunk or cleared list must not leave stale
/// items behind). Unrecognized leaves / shapes are a clean no-op. `status` is
/// normalized to `todo|in_progress|done`. `updated_at` is the event ts.
fn fold_tasks(
    conn: &Connection,
    session: &str,
    leaf: &str,
    payload: &Value,
    updated_at: &str,
) -> rusqlite::Result<()> {
    // Locate the raw items field by harness leaf; anything else is not a task event.
    let raw = match leaf {
        "assistant/todo" => payload.get("items"),
        "tool/TodoWrite/result" | "tool/TodoWrite/call" => {
            payload.get("input").or_else(|| payload.get("tool_input"))
        }
        _ => return Ok(()),
    };
    let Some(items) = raw.and_then(extract_items) else {
        return Ok(()); // absent / unrecognized shape — skip, mutate nothing
    };
    // Upsert each readable item; collect the ids we kept so stale rows can be reaped.
    let mut seen: Vec<String> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let Some((text, status)) = item_text_status(item) else {
            continue; // an item with no readable text — skip it
        };
        let item_id = item_id_of(item, idx);
        conn.execute(
            "INSERT INTO code_session_tasks (elanus_session, item_id, text, status, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(elanus_session, item_id) DO UPDATE SET
               text=excluded.text, status=excluded.status, updated_at=excluded.updated_at",
            params![session, item_id, text, status, updated_at],
        )?;
        seen.push(item_id);
    }
    if seen.is_empty() {
        // A genuinely EMPTY list = the agent cleared its tasks: drop the session's
        // rows. A non-empty-but-all-unreadable list is treated as unrecognized and
        // left untouched (do not wipe a real list on a shape we failed to read).
        if items.is_empty() {
            conn.execute(
                "DELETE FROM code_session_tasks WHERE elanus_session = ?1",
                params![session],
            )?;
        }
        return Ok(());
    }
    // Drop prior rows for this session absent from the latest list.
    let placeholders = (0..seen.len())
        .map(|i| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "DELETE FROM code_session_tasks WHERE elanus_session = ?1 AND item_id NOT IN ({placeholders})"
    );
    let mut binds: Vec<String> = Vec::with_capacity(seen.len() + 1);
    binds.push(session.to_string());
    binds.extend(seen.iter().cloned());
    conn.execute(&sql, rusqlite::params_from_iter(binds.iter()))?;
    Ok(())
}

/// Resolve a todo-list field (often a serialized JSON string, see `clip_value`) to
/// the array of item values: a direct array, or the array under a recognized key on
/// an object (`todos`/`items`/`tasks`/…). None when no array can be found.
fn extract_items(raw: &Value) -> Option<Vec<Value>> {
    let parsed: Value = match raw {
        Value::String(s) => serde_json::from_str(s).ok()?,
        other => other.clone(),
    };
    if let Some(arr) = parsed.as_array() {
        return Some(arr.clone());
    }
    if let Some(obj) = parsed.as_object() {
        for key in ["todos", "items", "tasks", "list", "todo", "plan"] {
            if let Some(arr) = obj.get(key).and_then(Value::as_array) {
                return Some(arr.clone());
            }
        }
    }
    None
}

/// Extract `(text, status)` from one task item. Handles a bare string item (the
/// string IS the text, status `todo`) and an object with a text-ish field plus a
/// status/state string or a `completed`/`done` bool. None when no readable text.
fn item_text_status(item: &Value) -> Option<(String, String)> {
    if let Some(s) = item.as_str() {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        return Some((t.to_string(), "todo".to_string()));
    }
    let obj = item.as_object()?;
    let text = [
        "content",
        "text",
        "step",
        "title",
        "name",
        "description",
        "task",
        "label",
    ]
    .iter()
    .find_map(|k| obj.get(*k).and_then(Value::as_str))
    .map(str::trim)
    .filter(|s| !s.is_empty())?
    .to_string();
    Some((text, normalize_status(obj)))
}

/// Normalize a task item's status to `todo|in_progress|done`. Reads a `status`/`state`
/// string (mapping the common synonyms each harness uses) or a `completed`/`done`
/// bool; an unknown status string is kept (lowercased) rather than guessed; absent
/// status defaults to `todo`.
fn normalize_status(obj: &serde_json::Map<String, Value>) -> String {
    if let Some(s) = obj
        .get("status")
        .or_else(|| obj.get("state"))
        .and_then(Value::as_str)
    {
        let s = s.trim().to_ascii_lowercase();
        return match s.as_str() {
            "in_progress" | "in-progress" | "inprogress" | "running" | "active" | "doing"
            | "started" => "in_progress".to_string(),
            "completed" | "complete" | "done" | "finished" | "closed" => "done".to_string(),
            "pending" | "todo" | "to_do" | "not_started" | "queued" | "open" | "" => {
                "todo".to_string()
            }
            _ => s, // unknown status: keep the raw lowercased value (advisory)
        };
    }
    if let Some(b) = obj
        .get("completed")
        .or_else(|| obj.get("done"))
        .and_then(Value::as_bool)
    {
        return if b { "done" } else { "todo" }.to_string();
    }
    "todo".to_string()
}

/// The stable per-item key: the item's own `id` (string or number) when present and
/// non-empty, else the zero-padded list index. Stable across events so latest-wins
/// upserts land on the same row.
fn item_id_of(item: &Value, idx: usize) -> String {
    if let Some(obj) = item.as_object() {
        let id = match obj.get("id") {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        if !id.is_empty() {
            return id;
        }
    }
    format!("{idx:04}")
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
    // SI2/SI3 read+write the shared coding-session tables owned by db.rs
    // (`code_session_tasks`, `code_claims`, `code_sessions`); ensure they exist so
    // the fold is self-sufficient even if the projection runs before any other
    // codesession path has initialized the full schema. Idempotent.
    crate::db::init_schema(&conn)?;
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
        // The projection only folds obs/agent/<noun>/<session>/… lines (durable
        // coding-session state). Edit-claims for hookless codex/opencode siblings are
        // NOT derived here: the obs/fs write camera only brackets CAGED actors, never
        // the un-caged coding harnesses (codeagent.rs SOURCE DECISION), so it never
        // witnesses their edits. They are auto-claimed from each harness's OWN
        // write-tool events instead (`auto_claim_write`: codex `file_change`, opencode
        // `edit`/`write`, claude hook) — a separate, already-wired locus.
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

// ── Read API: queries over the projection (for the CLI + web relay) ───────────

/// The `code_session_stats` columns, in the order `row_to_stat` expects. Shared by
/// the list, detail, and children queries so the projection→struct mapping stays
/// in one place.
const STATS_COLUMNS: &str = "elanus_session, tool, agent_noun, native_session, workdir, model, \
effort, parent, started_at, ended_at, exit_code, last_status, resume_count, input_tokens, \
output_tokens, updated_at, args";

/// Milliseconds between two RFC3339 timestamps, or None if either fails to parse.
fn duration_ms_between(start: &str, end: &str) -> Option<i64> {
    let s = DateTime::parse_from_rfc3339(start).ok()?;
    let e = DateTime::parse_from_rfc3339(end).ok()?;
    Some((e - s).num_milliseconds())
}

/// Ordering rank: active sessions (running/idle) first, finished next, unknown
/// last. Within a rank the caller sorts by `started_at` descending (newest first).
fn status_rank(status: Option<&str>) -> u8 {
    match status {
        Some("running") | Some("idle") => 0,
        Some("done") => 1,
        _ => 2,
    }
}

/// Map a `code_session_stats` row (selected via `STATS_COLUMNS`) to a `SessionStat`,
/// deriving `duration_ms` from the start/end timestamps when both are present.
fn row_to_stat(row: &rusqlite::Row) -> rusqlite::Result<SessionStat> {
    let started_at: Option<String> = row.get(8)?;
    let ended_at: Option<String> = row.get(9)?;
    let duration_ms = match (started_at.as_deref(), ended_at.as_deref()) {
        (Some(s), Some(e)) => duration_ms_between(s, e),
        _ => None,
    };
    let elanus_session: String = row.get(0)?;
    let resume_count: i64 = row.get(12)?;
    Ok(SessionStat {
        // Per-incarnation defaults for the additive fold fields; `fold_threads`
        // overwrites them on a grouped representative row.
        incarnations: vec![elanus_session.clone()],
        relaunches: 0,
        driven_resumes: resume_count,
        elanus_session,
        tool: row.get(1)?,
        agent_noun: row.get(2)?,
        native_session: row.get(3)?,
        workdir: row.get(4)?,
        model: row.get(5)?,
        effort: row.get(6)?,
        parent: row.get(7)?,
        started_at,
        ended_at,
        exit_code: row.get(10)?,
        last_status: row.get(11)?,
        resume_count: row.get(12)?,
        input_tokens: row.get(13)?,
        output_tokens: row.get(14)?,
        updated_at: row.get(15)?,
        duration_ms,
        // Backfilled from `code_sessions.intent` by `fill_intent` (the neighbor
        // idiom `fill_native` follows) — not a `code_session_stats` column.
        intent: None,
        args: row.get(16)?,
        launched_by_event: None,
    })
}

/// Active-first then newest-started ordering, applied in place. Shared by the raw
/// listing and the grouped (thread) listing so both surface the same order.
fn sort_active_first(out: &mut [SessionStat]) {
    out.sort_by(|a, b| {
        status_rank(a.last_status.as_deref())
            .cmp(&status_rank(b.last_status.as_deref()))
            .then_with(|| b.started_at.cmp(&a.started_at))
    });
}

/// Every per-incarnation `code_session_stats` row, native_session filled from the
/// durable `code_sessions` mapping when the obs-derived copy is null. This is the
/// UNGROUPED view (`lanius code sessions --raw`): one row per launch, the old
/// behaviour. Robust to the projection tables not existing yet (empty list).
pub fn list_sessions_raw(root: &Root) -> Result<Vec<SessionStat>> {
    let conn = crate::db::open(root)?;
    let sql = format!("SELECT {STATS_COLUMNS} FROM code_session_stats");
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Ok(Vec::new()), // projection table not created yet
    };
    let mut out: Vec<SessionStat> = stmt
        .query_map([], row_to_stat)?
        .filter_map(Result::ok)
        .collect();
    let overrides = native_overrides(&conn);
    let intents = intent_overrides(&conn);
    let launch_edges = launched_by_event_overrides(&conn);
    for s in &mut out {
        fill_native(s, &overrides);
        fill_intent(s, &intents);
        fill_launched_by_event(s, &launch_edges);
    }
    sort_active_first(&mut out);
    Ok(out)
}

/// The durable elanus_session→native_session mapping from `code_sessions`
/// (codesession.rs), used to fill `native_session` when the obs-derived projection
/// copy is null (an incarnation whose native id was recorded by the launcher but
/// never reached the projection as a `session/thread`/`session/started` obs line).
/// Best-effort: a missing table / query error yields an empty map (the fold simply
/// falls back to `elanus_session` as the thread key, which stays correct).
fn native_overrides(conn: &Connection) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT elanus_session, native_session FROM code_sessions \
         WHERE native_session IS NOT NULL AND native_session <> ''",
    ) else {
        return map;
    };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    else {
        return map;
    };
    for (es, ns) in rows.flatten() {
        map.insert(es, ns);
    }
    map
}

/// Fill a stat's `native_session` from the durable override when the obs-derived
/// copy is null/empty (the robustness LEFT JOIN the handoff calls out).
fn fill_native(s: &mut SessionStat, overrides: &std::collections::HashMap<String, String>) {
    let empty = s
        .native_session
        .as_deref()
        .map(str::is_empty)
        .unwrap_or(true);
    if empty {
        if let Some(ns) = overrides.get(&s.elanus_session) {
            s.native_session = Some(ns.clone());
        }
    }
}

/// worker-legibility M1: the durable elanus_session→intent mapping from
/// `code_sessions` (codesession.rs owns the column), used to backfill
/// `SessionStat.intent` — the projection has no intent column of its own. Mirrors
/// `native_overrides` exactly. Blank/whitespace-only intent is treated as absent
/// (matches codesession.rs's own `set_intent`/read filtering). Best-effort: a
/// missing table / query error yields an empty map.
fn intent_overrides(conn: &Connection) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT elanus_session, intent FROM code_sessions \
         WHERE intent IS NOT NULL AND trim(intent) <> ''",
    ) else {
        return map;
    };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    else {
        return map;
    };
    for (es, intent) in rows.flatten() {
        map.insert(es, intent);
    }
    map
}

/// Fill a stat's `intent` from the durable baseline-intent mapping. Mirrors
/// `fill_native`.
fn fill_intent(s: &mut SessionStat, overrides: &std::collections::HashMap<String, String>) {
    if let Some(intent) = overrides.get(&s.elanus_session) {
        s.intent = Some(intent.clone());
    }
}

fn launched_by_event_overrides(conn: &Connection) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT elanus_session, launched_by_event FROM code_sessions \
         WHERE launched_by_event IS NOT NULL AND trim(launched_by_event) <> ''",
    ) else {
        return map;
    };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    else {
        return map;
    };
    for (es, event) in rows.flatten() {
        map.insert(es, event);
    }
    map
}

fn fill_launched_by_event(
    s: &mut SessionStat,
    overrides: &std::collections::HashMap<String, String>,
) {
    if let Some(event) = overrides.get(&s.elanus_session) {
        s.launched_by_event = Some(event.clone());
    }
}

/// The grouping key for a stat: its native thread id, or its lanius id as a
/// fallback when the native id is unknown (an incarnation with no native id stays
/// 1:1, which is correct — we cannot claim it is the same thread). native_session
/// is effectively globally unique, so it is a safe collapse key.
fn thread_key(s: &SessionStat) -> String {
    match s.native_session.as_deref() {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => s.elanus_session.clone(),
    }
}

/// Sort incarnations of one thread newest-first by (started_at, updated_at) then
/// elanus_session as a stable tiebreaker. The first element after this sort is the
/// representative (latest) incarnation.
fn incarnation_cmp(a: &SessionStat, b: &SessionStat) -> std::cmp::Ordering {
    b.started_at
        .cmp(&a.started_at)
        .then_with(|| b.updated_at.cmp(&a.updated_at))
        .then_with(|| b.elanus_session.cmp(&a.elanus_session))
}

/// Fold per-incarnation stats into one representative stat PER `thread_key`,
/// remapping parent edges into thread-space (TG1 + TG2). The returned rows keep
/// the WIRE id-space the UI already understands: each thread is represented by its
/// LATEST incarnation's `elanus_session` (the row id), and `parent` is set to the
/// parent thread's representative `elanus_session` (never a raw native id the UI
/// has never seen) so the existing tree-linking code stays valid.
fn fold_threads(stats: Vec<SessionStat>) -> Vec<SessionStat> {
    use std::collections::HashMap;

    // Group incarnations by thread key.
    let mut groups: HashMap<String, Vec<SessionStat>> = HashMap::new();
    for s in stats {
        groups.entry(thread_key(&s)).or_default().push(s);
    }

    // Map every incarnation's elanus_session → its thread key, so we can remap
    // parent edges (a parent edge points at an incarnation's lanius id) into the
    // thread it belongs to (TG2).
    let mut elanus_to_thread: HashMap<String, String> = HashMap::new();
    for (key, incs) in &groups {
        for inc in incs {
            elanus_to_thread.insert(inc.elanus_session.clone(), key.clone());
        }
    }

    // Fold each group to a representative row.
    let mut reps: HashMap<String, SessionStat> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (key, mut incs) in groups {
        incs.sort_by(incarnation_cmp); // newest first
        let mut rep = incs[0].clone(); // representative = latest incarnation

        // started_at = MIN, last_active/updated_at = MAX across incarnations.
        rep.started_at = incs.iter().filter_map(|i| i.started_at.clone()).min();
        rep.updated_at = incs.iter().filter_map(|i| i.updated_at.clone()).max();
        // Sum the fungible counters.
        rep.input_tokens = incs.iter().map(|i| i.input_tokens).sum();
        rep.output_tokens = incs.iter().map(|i| i.output_tokens).sum();
        // Resumes reported HONESTLY and SEPARATELY: relaunches = manual
        // fresh-id incarnations folded here; driven_resumes = Σ daemon resume_count.
        rep.driven_resumes = incs.iter().map(|i| i.resume_count).sum();
        rep.relaunches = incs.len() as i64 - 1;
        rep.incarnations = incs.iter().map(|i| i.elanus_session.clone()).collect();
        // Recompute duration from the folded min-start / max-updated when ended.
        rep.duration_ms = match (rep.started_at.as_deref(), rep.ended_at.as_deref()) {
            (Some(s), Some(e)) => duration_ms_between(s, e),
            _ => None,
        };

        order.push(key.clone());
        reps.insert(key, rep);
    }

    // TG2: remap each thread's parent edge to the parent THREAD's representative
    // elanus_session (the wire id the UI keys on). A parent whose thread is absent
    // becomes a root (parent cleared).
    let parent_rep: HashMap<String, String> = reps
        .iter()
        .map(|(k, r)| (k.clone(), r.elanus_session.clone()))
        .collect();
    for key in &order {
        let parent_thread = reps
            .get(key)
            .and_then(|r| r.parent.as_deref())
            .and_then(|p| elanus_to_thread.get(p))
            .cloned();
        let rep = reps.get_mut(key).expect("rep present");
        // Self-parent can arise if an incarnation's parent edge points at another
        // incarnation of the SAME thread — that is not a real spawn edge, drop it.
        rep.parent = match parent_thread {
            Some(pt) if &pt != key => parent_rep.get(&pt).cloned(),
            _ => None,
        };
    }

    let mut out: Vec<SessionStat> = order.into_iter().filter_map(|k| reps.remove(&k)).collect();
    sort_active_first(&mut out);
    out
}

/// List coding sessions GROUPED into logical threads (default). Manual `--resume`
/// relaunches that mint a fresh lanius id but continue the same native thread fold
/// into ONE row per `thread_key`; the spawn tree is remapped into thread-space so
/// planner→worker stays nested while resume-incarnations collapse to one node.
/// Robust to the projection tables not existing yet (returns an empty list).
pub fn list_sessions(root: &Root) -> Result<Vec<SessionStat>> {
    Ok(fold_threads(list_sessions_raw(root)?))
}

/// One THREAD's detail: the folded representative stat, the UNION event timeline
/// across all its incarnations (ordered by ts then event id, each event still
/// labeled by its source incarnation), a paste-able resume command targeting the
/// latest incarnation, and the thread's child threads. `id` may be EITHER an
/// `elanus_session` (any incarnation) OR a raw `thread_key` (native id) — both
/// resolve to the same thread. None when the id is not in the projection (or no
/// projection exists yet).
pub fn session_detail(root: &Root, id: &str) -> Result<Option<SessionDetail>> {
    let conn = crate::db::open(root)?;
    // Confirm the projection exists; an absent table means "no such session".
    if conn
        .prepare(&format!(
            "SELECT {STATS_COLUMNS} FROM code_session_stats LIMIT 0"
        ))
        .is_err()
    {
        return Ok(None);
    }

    // Build the grouped view so we can resolve `id` to a thread and reuse the fold
    // (representative stat, remapped parent, summed counters). One implementation,
    // shared with the listing.
    let threads = list_sessions(root)?;
    let Some(session) = threads.into_iter().find(|t| {
        t.elanus_session == id || t.incarnations.iter().any(|i| i == id) || {
            // Resolve a raw thread_key (native id) too.
            t.native_session.as_deref() == Some(id)
        }
    }) else {
        return Ok(None);
    };

    // The incarnation ids that make up this thread — the union timeline spans them.
    let incarnation_ids = session.incarnations.clone();
    let placeholders = incarnation_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(",");

    // UNION timeline across every incarnation, ordered by ts then event id, each
    // event still labeled by its source incarnation (the `elanus_session` field).
    let ev_sql = format!(
        "SELECT id, elanus_session, ts, kind, summary, created_at \
         FROM code_session_events WHERE elanus_session IN ({placeholders}) \
         ORDER BY ts, id"
    );
    let mut ev_stmt = conn.prepare(&ev_sql)?;
    let ev_params = rusqlite::params_from_iter(incarnation_ids.iter());
    let events: Vec<SessionEvent> = ev_stmt
        .query_map(ev_params, |row| {
            Ok(SessionEvent {
                id: row.get(0)?,
                elanus_session: row.get(1)?,
                ts: row.get(2)?,
                kind: row.get(3)?,
                summary: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?
        .filter_map(Result::ok)
        .collect();

    // Children: child THREADS whose remapped parent is this thread's wire id. We
    // fold the full listing again and select those parented here, so the detail's
    // children are threads (not raw incarnations) consistent with the tree.
    let children: Vec<SessionStat> = list_sessions(root)?
        .into_iter()
        .filter(|t| t.parent.as_deref() == Some(session.elanus_session.as_str()))
        .collect();
    // The resume hint targets the live native thread of the latest incarnation,
    // expressed per-tool (managed passthrough). None when the tool has no clean
    // passthrough resume — the UI then omits the hint entirely.
    let resume_command = match (session.tool.as_deref(), session.native_session.as_deref()) {
        (Some(tool), Some(native)) => crate::codeagent::interactive_resume_hint(tool, native),
        _ => None,
    };

    Ok(Some(SessionDetail {
        session,
        events,
        resume_command,
        children,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn temp_root(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!(
            "lanius-code-projection-{tag}-{}",
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
        let row: (
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
            i64,
            Option<String>,
            Option<i64>,
        ) = conn
            .query_row(
                "SELECT parent, model, effort, input_tokens, output_tokens, last_status, exit_code
                 FROM code_session_stats WHERE elanus_session='code-test123'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
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

    // ── session-thread-grouping (TG1/TG2) ─────────────────────────────────────

    /// Emit a CC incarnation: a `session/start` (fresh lanius id), a
    /// `session/started` carrying the shared `cc_session` (native thread id), one
    /// tool/idle event, and a stop. `parent` is optional (the spawn edge).
    fn cc_incarnation(
        root: &Root,
        lanius: &str,
        cc_session: &str,
        parent: Option<&str>,
        t0: &str,
        in_tok: i64,
        out_tok: i64,
    ) {
        let mut start = json!({ "ts": format!("{t0}.000Z"), "tool": "claude" });
        if let Some(p) = parent {
            start["parent"] = json!(p);
        }
        append_trace(
            root,
            &format!("obs/agent/claude-code/{lanius}/session/start"),
            start,
        );
        append_trace(
            root,
            &format!("obs/agent/claude-code/{lanius}/session/started"),
            json!({ "ts": format!("{t0}.100Z"), "cc_session": cc_session }),
        );
        append_trace(
            root,
            &format!("obs/agent/claude-code/{lanius}/tool/edit/call"),
            json!({ "ts": format!("{t0}.200Z"), "command": format!("edit by {lanius}") }),
        );
        append_trace(
            root,
            &format!("obs/agent/claude-code/{lanius}/session/idle"),
            json!({ "ts": format!("{t0}.300Z"), "usage": { "input_tokens": in_tok, "output_tokens": out_tok } }),
        );
        append_trace(
            root,
            &format!("obs/agent/claude-code/{lanius}/session/stop"),
            json!({ "ts": format!("{t0}.400Z"), "exit_code": 0 }),
        );
    }

    #[test]
    fn tg1_three_incarnations_fold_to_one_thread() {
        let root = temp_root("tg1-fold");
        // Three manual --resume relaunches: fresh lanius id each, SAME cc_session.
        cc_incarnation(
            &root,
            "code-aaa",
            "cc-thread-1",
            None,
            "2026-06-20T00:00:00",
            10,
            1,
        );
        cc_incarnation(
            &root,
            "code-bbb",
            "cc-thread-1",
            None,
            "2026-06-20T01:00:00",
            20,
            2,
        );
        cc_incarnation(
            &root,
            "code-ccc",
            "cc-thread-1",
            None,
            "2026-06-20T02:00:00",
            30,
            3,
        );
        // A second daemon-driven resume on the latest incarnation.
        append_trace(
            &root,
            "obs/agent/claude-code/code-ccc/session/resume",
            json!({ "ts": "2026-06-20T02:30:00.000Z" }),
        );
        project_trace(&root).unwrap();

        let threads = list_sessions(&root).unwrap();
        assert_eq!(threads.len(), 1, "three incarnations fold to ONE thread");
        let t = &threads[0];
        // Representative wire id = latest incarnation, incarnations newest-first.
        assert_eq!(t.elanus_session, "code-ccc");
        assert_eq!(t.incarnations, vec!["code-ccc", "code-bbb", "code-aaa"]);
        assert_eq!(t.native_session.as_deref(), Some("cc-thread-1"));
        // started_at = MIN, updated_at = MAX across incarnations.
        assert_eq!(t.started_at.as_deref(), Some("2026-06-20T00:00:00.000Z"));
        assert_eq!(t.updated_at.as_deref(), Some("2026-06-20T02:30:00.000Z"));
        // Tokens summed.
        assert_eq!(t.input_tokens, 60);
        assert_eq!(t.output_tokens, 6);
        // Resumes reported SEPARATELY: relaunches = incarnations-1, driven = Σ.
        assert_eq!(t.relaunches, 2);
        assert_eq!(t.driven_resumes, 1, "one session/resume drove resume_count");

        // The raw (ungrouped) view still exposes all three incarnations.
        assert_eq!(list_sessions_raw(&root).unwrap().len(), 3);
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn tg1_null_native_incarnation_stays_one_to_one() {
        let root = temp_root("tg1-null");
        // An incarnation that started but never produced a native id stays 1:1.
        append_trace(
            &root,
            "obs/agent/claude-code/code-nonative/session/start",
            json!({ "ts": "2026-06-20T00:00:00.000Z", "tool": "claude" }),
        );
        cc_incarnation(
            &root,
            "code-withnative",
            "cc-x",
            None,
            "2026-06-20T01:00:00",
            5,
            1,
        );
        project_trace(&root).unwrap();

        let threads = list_sessions(&root).unwrap();
        assert_eq!(
            threads.len(),
            2,
            "the null-native incarnation does not fold"
        );
        let null_thread = threads
            .iter()
            .find(|t| t.elanus_session == "code-nonative")
            .unwrap();
        assert_eq!(null_thread.incarnations, vec!["code-nonative"]);
        assert_eq!(null_thread.relaunches, 0);
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn tg1_detail_unions_timeline_by_any_incarnation_id() {
        let root = temp_root("tg1-detail");
        cc_incarnation(
            &root,
            "code-i1",
            "cc-union",
            None,
            "2026-06-20T00:00:00",
            1,
            1,
        );
        cc_incarnation(
            &root,
            "code-i2",
            "cc-union",
            None,
            "2026-06-20T01:00:00",
            1,
            1,
        );
        project_trace(&root).unwrap();

        // Resolve by the OLDER incarnation id, the LATEST id, and the native key —
        // all return the same unioned, ts-ordered timeline.
        for id in ["code-i1", "code-i2", "cc-union"] {
            let d = session_detail(&root, id).unwrap().expect("thread resolves");
            assert_eq!(d.session.elanus_session, "code-i2", "rep is latest");
            // Two tool/edit events, one per incarnation, ts-ordered.
            let edits: Vec<&SessionEvent> = d
                .events
                .iter()
                .filter(|e| e.kind.as_deref() == Some("tool/edit/call"))
                .collect();
            assert_eq!(edits.len(), 2, "union spans both incarnations for id {id}");
            assert!(
                edits[0].ts <= edits[1].ts,
                "timeline ordered by ts for id {id}"
            );
            // Each event still labeled by its source incarnation.
            assert_eq!(edits[0].elanus_session, "code-i1");
            assert_eq!(edits[1].elanus_session, "code-i2");
            // The resume hint is the per-tool managed relaunch targeting the
            // (shared) native thread, not an lanius verb — `lanius code claude
            // --resume <native>`.
            assert_eq!(
                d.resume_command.as_deref(),
                Some("lanius code claude --resume cc-union"),
                "per-tool interactive-resume hint for id {id}"
            );
        }
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn tg2_worker_resumes_collapse_under_planner() {
        let root = temp_root("tg2-tree");
        // A planner spawns a worker; the worker is then manually resumed twice
        // (three incarnations, one native thread), each carrying the planner edge.
        cc_incarnation(
            &root,
            "code-planner",
            "cc-planner",
            None,
            "2026-06-20T00:00:00",
            1,
            1,
        );
        cc_incarnation(
            &root,
            "code-w1",
            "cc-worker",
            Some("code-planner"),
            "2026-06-20T00:10:00",
            1,
            1,
        );
        cc_incarnation(
            &root,
            "code-w2",
            "cc-worker",
            Some("code-planner"),
            "2026-06-20T00:20:00",
            1,
            1,
        );
        cc_incarnation(
            &root,
            "code-w3",
            "cc-worker",
            Some("code-planner"),
            "2026-06-20T00:30:00",
            1,
            1,
        );
        project_trace(&root).unwrap();

        let threads = list_sessions(&root).unwrap();
        // Two threads: the planner and the (folded) worker — NOT four.
        assert_eq!(threads.len(), 2);
        let worker = threads
            .iter()
            .find(|t| t.native_session.as_deref() == Some("cc-worker"))
            .unwrap();
        let planner = threads
            .iter()
            .find(|t| t.native_session.as_deref() == Some("cc-planner"))
            .unwrap();
        assert_eq!(
            worker.incarnations.len(),
            3,
            "three resumes fold to one worker node"
        );
        // The worker's parent edge points at the planner's representative wire id
        // (an elanus_session the UI already keys on), NOT a raw native id.
        assert_eq!(
            worker.parent.as_deref(),
            Some(planner.elanus_session.as_str())
        );

        // Roots (parentless threads) = just the planner. The worker is NOT a root,
        // and three resumes did NOT produce three roots.
        let roots: Vec<&SessionStat> = threads.iter().filter(|t| t.parent.is_none()).collect();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].elanus_session, planner.elanus_session);

        // The planner's detail lists the worker THREAD as its child (one, not three).
        let pdetail = session_detail(&root, &planner.elanus_session)
            .unwrap()
            .unwrap();
        assert_eq!(pdetail.children.len(), 1);
        assert_eq!(
            pdetail.children[0].native_session.as_deref(),
            Some("cc-worker")
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn backward_compat_representative_carries_all_original_fields() {
        let root = temp_root("tg-compat");
        cc_incarnation(
            &root,
            "code-rep",
            "cc-rep",
            None,
            "2026-06-20T00:00:00",
            7,
            3,
        );
        project_trace(&root).unwrap();
        let threads = list_sessions(&root).unwrap();
        let s = &threads[0];
        // Every original SessionStat field name is present and meaningful on the
        // representative row (the wire shape stays a superset).
        assert_eq!(s.elanus_session, "code-rep");
        assert_eq!(s.tool.as_deref(), Some("claude"));
        assert_eq!(s.agent_noun.as_deref(), Some("claude-code"));
        assert_eq!(s.native_session.as_deref(), Some("cc-rep"));
        assert_eq!(s.last_status.as_deref(), Some("done"));
        assert_eq!(s.exit_code, Some(0));
        assert!(s.started_at.is_some());
        assert!(s.ended_at.is_some());
        assert!(s.duration_ms.is_some());
        assert_eq!(s.input_tokens, 7);
        assert_eq!(s.output_tokens, 3);
        // Serialized JSON includes both the original and the additive fields.
        let v = serde_json::to_value(s).unwrap();
        for field in [
            "elanus_session",
            "tool",
            "native_session",
            "resume_count",
            "duration_ms",
        ] {
            assert!(
                v.get(field).is_some(),
                "original field {field} present in wire"
            );
        }
        for field in ["incarnations", "relaunches", "driven_resumes"] {
            assert!(
                v.get(field).is_some(),
                "additive field {field} present in wire"
            );
        }
        std::fs::remove_dir_all(&root.dir).ok();
    }

    // ── sibling-intent (SI2: per-session task list projection) ────────────────

    #[test]
    fn si2_codex_todo_folds_and_selects_in_progress() {
        let root = temp_root("si2-codex");
        // codex `assistant/todo`: the harness clips `items` with `clip_value`, which
        // SERIALIZES the array to a JSON STRING — fold it through that real shape.
        let items = json!([
            { "content": "step 1", "status": "completed" },
            { "content": "step 2", "status": "in_progress" },
            { "content": "step 3", "status": "pending" }
        ])
        .to_string();
        append_trace(
            &root,
            "obs/agent/codex/code-si2codex/assistant/todo",
            json!({ "ts": "2026-06-28T00:00:00.000Z", "items": items }),
        );
        project_trace(&root).unwrap();

        // Three rows, normalized statuses, in_progress selected as the current task.
        let conn = crate::db::open(&root).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM code_session_tasks WHERE elanus_session='code-si2codex'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 3);
        let cur = crate::codesession::current_task(&root, "code-si2codex").unwrap();
        assert_eq!(cur, ("step 2".to_string(), "in_progress".to_string()));

        // Latest-wins + stale-reap: a shorter follow-up list drops the removed item.
        let items2 = json!([{ "content": "only step", "status": "in_progress" }]).to_string();
        append_trace(
            &root,
            "obs/agent/codex/code-si2codex/assistant/todo",
            json!({ "ts": "2026-06-28T00:01:00.000Z", "items": items2 }),
        );
        project_trace(&root).unwrap();
        let n2: i64 = conn
            .query_row(
                "SELECT count(*) FROM code_session_tasks WHERE elanus_session='code-si2codex'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n2, 1, "the shrunk list reaps the removed items");
        let cur2 = crate::codesession::current_task(&root, "code-si2codex").unwrap();
        assert_eq!(cur2, ("only step".to_string(), "in_progress".to_string()));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn si2_claude_todowrite_folds_from_stringified_input() {
        let root = temp_root("si2-claude");
        // Claude TodoWrite's obs `input` is the clip_value-stringified tool_input,
        // i.e. a JSON STRING of `{ "todos": [ { content, status } ] }`.
        let input = json!({ "todos": [
            { "content": "write the fold", "status": "in_progress" },
            { "content": "ship it", "status": "pending" }
        ] })
        .to_string();
        append_trace(
            &root,
            "obs/agent/claude-code/code-si2cc/tool/TodoWrite/result",
            json!({ "ts": "2026-06-28T00:00:00.000Z", "tool": "TodoWrite", "input": input }),
        );
        project_trace(&root).unwrap();
        let cur = crate::codesession::current_task(&root, "code-si2cc").unwrap();
        assert_eq!(
            cur,
            ("write the fold".to_string(), "in_progress".to_string())
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    // ── worker-legibility M1: launch identity (intent + args) ─────────────────

    #[test]
    fn m1_intent_backfilled_from_code_sessions_on_list_and_detail() {
        let root = temp_root("m1-intent");
        append_trace(
            &root,
            "obs/agent/codex/code-m1intent/session/start",
            json!({ "ts": "2026-07-12T00:00:00.000Z", "tool": "codex" }),
        );
        project_trace(&root).unwrap();
        crate::codesession::set_intent(&root, "code-m1intent", "fix the flaky e2e test").unwrap();

        let listed = list_sessions(&root).unwrap();
        let s = listed
            .iter()
            .find(|s| s.elanus_session == "code-m1intent")
            .unwrap();
        assert_eq!(s.intent.as_deref(), Some("fix the flaky e2e test"));

        let detail = session_detail(&root, "code-m1intent").unwrap().unwrap();
        assert_eq!(
            detail.session.intent.as_deref(),
            Some("fix the flaky e2e test")
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn m1_args_column_round_trips_from_session_start() {
        let root = temp_root("m1-args");
        append_trace(
            &root,
            "obs/agent/codex/code-m1args/session/start",
            json!({
                "ts": "2026-07-12T00:00:00.000Z",
                "tool": "codex",
                "args": ["--dangerously-skip-permissions", "fix the bug"]
            }),
        );
        project_trace(&root).unwrap();

        let listed = list_sessions_raw(&root).unwrap();
        let s = listed
            .iter()
            .find(|s| s.elanus_session == "code-m1args")
            .unwrap();
        let args: Value = serde_json::from_str(s.args.as_deref().unwrap()).unwrap();
        assert_eq!(
            args,
            json!(["--dangerously-skip-permissions", "fix the bug"])
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn m1_missing_model_effort_stay_null_on_the_wire() {
        let root = temp_root("m1-nullmodel");
        append_trace(
            &root,
            "obs/agent/codex/code-m1null/session/start",
            json!({ "ts": "2026-07-12T00:00:00.000Z", "tool": "codex" }),
        );
        project_trace(&root).unwrap();

        let listed = list_sessions(&root).unwrap();
        let s = listed
            .iter()
            .find(|s| s.elanus_session == "code-m1null")
            .unwrap();
        assert_eq!(s.model, None);
        assert_eq!(s.effort, None);
        let v = serde_json::to_value(s).unwrap();
        assert!(v.get("model").unwrap().is_null());
        assert!(v.get("effort").unwrap().is_null());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn m2_launched_by_event_backfilled_from_code_sessions_on_list_and_detail() {
        let root = temp_root("m2-launched-by");
        append_trace(
            &root,
            "obs/agent/codex/code-m2edge/session/start",
            json!({ "ts": "2026-07-12T00:00:00.000Z", "tool": "codex" }),
        );
        project_trace(&root).unwrap();
        crate::codesession::set_launched_by_event(&root, "code-m2edge", "code-spawn-corr").unwrap();

        let listed = list_sessions(&root).unwrap();
        let s = listed
            .iter()
            .find(|s| s.elanus_session == "code-m2edge")
            .unwrap();
        assert_eq!(s.launched_by_event.as_deref(), Some("code-spawn-corr"));

        let detail = session_detail(&root, "code-m2edge").unwrap().unwrap();
        assert_eq!(
            detail.session.launched_by_event.as_deref(),
            Some("code-spawn-corr")
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }
}
