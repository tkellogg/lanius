use crate::paths::Root;
use anyhow::Result;
use rusqlite::Connection;

pub fn open(root: &Root) -> Result<Connection> {
    let conn = Connection::open(root.db())?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(conn)
}

pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS events (
  id              INTEGER PRIMARY KEY,
  type            TEXT NOT NULL,
  cause_id        INTEGER REFERENCES events(id),
  correlation_id  TEXT,
  payload         TEXT,
  state           TEXT NOT NULL DEFAULT 'pending',
                  -- pending | running | done | failed | waiting_on_human | expired | denied
  -- Which handler invocation emitted this event (from HARNESS_DISPATCH_ID).
  -- Scopes suspend/resume: an ask is matched to the dispatch that asked it.
  emitted_by_dispatch INTEGER,
  priority        INTEGER NOT NULL DEFAULT 0,
  deadline        TEXT,
  default_action  TEXT,
  idempotency_key TEXT UNIQUE,
  created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  finished_at     TEXT
);
CREATE INDEX IF NOT EXISTS idx_events_pending ON events(state, type, priority);
CREATE INDEX IF NOT EXISTS idx_events_correlation ON events(correlation_id);

-- One row per handler invocation; the event-level state machine derives from these.
CREATE TABLE IF NOT EXISTS dispatches (
  id                 INTEGER PRIMARY KEY,
  event_id           INTEGER NOT NULL REFERENCES events(id),
  handler            TEXT NOT NULL,
  state              TEXT NOT NULL DEFAULT 'running', -- running | done | failed | suspended
  exit_code          INTEGER,
  resume_correlation TEXT,
  started_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  finished_at        TEXT
);
CREATE INDEX IF NOT EXISTS idx_dispatches_state ON dispatches(state);
CREATE INDEX IF NOT EXISTS idx_dispatches_event ON dispatches(event_id);

-- Transcripts: one row per message. The process state of a suspended exec.
CREATE TABLE IF NOT EXISTS messages (
  id         INTEGER PRIMARY KEY,
  session_id TEXT NOT NULL,
  role       TEXT NOT NULL,            -- user | assistant | tool
  content    TEXT NOT NULL,            -- JSON normalized message (incl. thinking, tool calls)
  event_id   INTEGER REFERENCES events(id),
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, id);

CREATE TABLE IF NOT EXISTS throttles (
  event_type          TEXT PRIMARY KEY,  -- MQTT filter, e.g. 'in/agent/#', 'signal/#'
  max_concurrent      INTEGER,
  rate_per_min        INTEGER,
  llm_tokens_per_hour INTEGER,
  coalesce            INTEGER NOT NULL DEFAULT 1  -- 0 = algedonic: never queue, never batch
);

-- Hook registrations (from [[hook]] in package manifests, crons-style).
-- The chain for a point runs ordered by ord, id; first deny stops it.
CREATE TABLE IF NOT EXISTS hooks (
  id           INTEGER PRIMARY KEY,
  skill        TEXT NOT NULL,
  point        TEXT NOT NULL,            -- pre_tool_call | post_tool_call | pre_dispatch
  run          TEXT NOT NULL,            -- path relative to the harness root
  ord          INTEGER NOT NULL DEFAULT 50,
  timeout_ms   INTEGER NOT NULL DEFAULT 500,
  on_timeout   TEXT NOT NULL DEFAULT 'deny',  -- also covers spawn errors
  match_filter TEXT NOT NULL DEFAULT '#',     -- MQTT filter on tool name / topic
  UNIQUE(skill, point, run)
);

CREATE TABLE IF NOT EXISTS crons (
  id         INTEGER PRIMARY KEY,
  skill      TEXT NOT NULL,
  schedule   TEXT NOT NULL,
  emit_type  TEXT NOT NULL,
  payload    TEXT,
  last_fired TEXT,
  UNIQUE(skill, emit_type, schedule)
);

-- The grants ledger: capability requests and decisions, pinned to manifest
-- hashes (docs/bus.md, Packages). Append-shaped: a revocation is a state
-- flip with provenance, never a deletion — the ledger reads as a capability
-- history. A package whose manifest changes gets fresh rows under the new
-- hash; values approved under the old hash carry over (decided_by =
-- 'carried'), the delta re-enters 'requested'.
CREATE TABLE IF NOT EXISTS grants (
  id            INTEGER PRIMARY KEY,
  package       TEXT NOT NULL,
  manifest_hash TEXT NOT NULL,   -- full version identity (manifest + code)
  code_hash     TEXT NOT NULL DEFAULT '', -- executables only; gates carry-over
  kind          TEXT NOT NULL,   -- subscribe | publish | blocking | fs_write
  value         TEXT NOT NULL,   -- topic filter / hook point / path prefix
  state         TEXT NOT NULL DEFAULT 'requested', -- requested | approved | revoked
  requested_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  decided_at    TEXT,
  decided_by    TEXT,            -- 'cli' | 'init' | 'carried'
  UNIQUE(package, manifest_hash, kind, value)
);
CREATE INDEX IF NOT EXISTS idx_grants_pkg ON grants(package, state);

-- Write leases: agent-acquired &mut on path prefixes, ⊆ the whole-agent
-- grant (docs/sandbox.md). The kernel is the borrow checker: no overlapping
-- active leases. Crash-only: released by the supervisor when the holder
-- dies; never leaks.
CREATE TABLE IF NOT EXISTS leases (
  id          INTEGER PRIMARY KEY,
  path        TEXT NOT NULL,     -- canonical absolute prefix
  session_id  TEXT,
  dispatch_id INTEGER,           -- set when held by a dispatched handler
  pid         INTEGER,           -- liveness check for standalone exec
  acquired_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  released_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_leases_active ON leases(released_at) WHERE released_at IS NULL;

CREATE TABLE IF NOT EXISTS kv (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS llm_usage (
  id            INTEGER PRIMARY KEY,
  event_id      INTEGER,
  root_type     TEXT,
  model         TEXT,
  input_tokens  INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
"#,
    )?;
    // Migrations for databases created before a column existed; the error on
    // a duplicate column is expected and ignored.
    let _ = conn.execute("ALTER TABLE events ADD COLUMN emitted_by_dispatch INTEGER", []);
    let _ = conn.execute("ALTER TABLE grants ADD COLUMN code_hash TEXT NOT NULL DEFAULT ''", []);
    Ok(())
}

pub fn kv_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM kv WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

pub fn kv_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO kv(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        [key, value],
    )?;
    Ok(())
}

pub fn kv_del(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM kv WHERE key = ?1", [key])?;
    Ok(())
}

/// Walk the cause chain to the root event and return its type.
/// Cost attribution and throttle policy key off this.
pub fn root_type(conn: &Connection, event_id: i64) -> Result<String> {
    let mut id = event_id;
    for _ in 0..64 {
        let (etype, cause): (String, Option<i64>) = conn.query_row(
            "SELECT type, cause_id FROM events WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        match cause {
            Some(c) => id = c,
            None => return Ok(etype),
        }
    }
    Ok("unknown".into())
}
