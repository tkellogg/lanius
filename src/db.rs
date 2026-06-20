use crate::paths::Root;
use anyhow::Result;
use rusqlite::Connection;

pub fn open(root: &Root) -> Result<Connection> {
    migrate_db_filename(root);
    let conn = Connection::open(root.db())?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(conn)
}

/// One-time, no-brick rename of the ledger file from the legacy `harness.db` to
/// `elanus.db` (the binary used to be `harness`). Runs at the single open
/// chokepoint BEFORE the connection is made — otherwise sqlite would create a
/// fresh empty elanus.db beside the old data. Moves the WAL/SHM siblings too so
/// no committed-but-unflushed transactions are lost. Idempotent and best-effort:
/// if the new name already exists, or there's nothing to move, it does nothing.
fn migrate_db_filename(root: &Root) {
    let new = root.db();
    let old = root.legacy_db();
    if new.exists() || !old.exists() {
        return;
    }
    for (from, to) in [
        (old.clone(), new.clone()),
        (sibling(&old, "-wal"), sibling(&new, "-wal")),
        (sibling(&old, "-shm"), sibling(&new, "-shm")),
    ] {
        if from.exists() {
            if let Err(e) = std::fs::rename(&from, &to) {
                eprintln!(
                    "[db] could not migrate {} -> {}: {e}",
                    from.display(),
                    to.display()
                );
            }
        }
    }
}

fn sibling(db: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut s = db.as_os_str().to_owned();
    s.push(suffix);
    std::path::PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Root;
    use sha2::{Digest, Sha256};

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn migrates_legacy_db_filename() {
        let dir = std::env::temp_dir().join(format!("el-dbmig-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let root = Root { dir: dir.clone() };
        // Seed an old-style harness.db with a row; no elanus.db present.
        {
            let conn = Connection::open(root.legacy_db()).unwrap();
            conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (42);")
                .unwrap();
        }
        assert!(root.legacy_db().exists() && !root.db().exists());
        // open() migrates the filename first, then opens the SAME data.
        let conn = open(&root).unwrap();
        assert!(root.db().exists(), "elanus.db must exist after migration");
        assert!(
            !root.legacy_db().exists(),
            "harness.db must be gone after migration"
        );
        let x: i64 = conn.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(x, 42, "data preserved across the rename");
        // Idempotent: a second open is a no-op (already migrated).
        drop(conn);
        let _ = open(&root).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn context_and_subagent_substrate_schema_accepts_records() {
        let dir = std::env::temp_dir().join(format!("el-dbctx-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let root = Root { dir: dir.clone() };
        let conn = open(&root).unwrap();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO context_blocks
             (scope, owner, session_id, run_id, name, placement, priority, package, content, content_sha256, meta)
             VALUES ('agent', 'main', NULL, NULL, 'identity', 'system', 10, 'core', 'hello', ?1, '{}')",
            [sha256_hex(b"hello")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO context_build_log
             (session_id, run_id, profile, agent, request_ordinal, component, action, block_name, after_sha256, summary, meta)
             VALUES ('s1', 'run1', 'default', 'main', 1, 'seed', 'add', 'identity', ?1, 'seeded identity', '{}')",
            [sha256_hex(b"hello")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO subagent_sessions
             (parent_session_id, child_session_id, parent_agent, child_agent, child_profile, inherited_budget, context_program, grant_policy)
             VALUES ('s1', 's1.child.1', 'main', 'scout', 'scout', 1, 'default', 'narrow')",
            [],
        )
        .unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM context_build_log WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let child: String = conn
            .query_row(
                "SELECT child_profile FROM subagent_sessions WHERE parent_session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(child, "scout");
        std::fs::remove_dir_all(&dir).ok();
    }
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
  -- Which handler invocation emitted this event (from ELANUS_DISPATCH_ID).
  -- Scopes suspend/resume: an ask is matched to the dispatch that asked it.
  emitted_by_dispatch INTEGER,
  priority        INTEGER NOT NULL DEFAULT 0,
  deadline        TEXT,
  default_action  TEXT,
  idempotency_key TEXT UNIQUE,
  -- Has this event been announced on the bus under its own topic?
  -- 0 = the daemon's announce sweep still owes it a bus publish; 1 = done
  -- (or it never needs one). Bus-origin events are inserted with 1 by the
  -- broker — it fans out itself at inbound time — which is what makes
  -- "announce exactly once" hold. DEFAULT 1 so rows inserted outside
  -- events::emit (and pre-migration rows, see ALTER below) are never
  -- blasted onto the bus retroactively; emit() always binds the value.
  announced       INTEGER NOT NULL DEFAULT 1,
  -- Who the kernel verified sent this event (docs/identity.md). For events
  -- that arrive over the bus, the broker sets this from the authenticated
  -- connection and overwrites anything the message tried to claim, so it is
  -- a fact the kernel vouches for, not a self-report. 'kernel' for events the
  -- kernel mints itself; the owner identity name (default 'owner') for the
  -- human's surfaces; the package name for a package actor. NULL only on rows
  -- written before this column existed.
  sender          TEXT,
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

-- Named, hashable context blocks. This is substrate only: the current prompt
-- path still materializes context::Doc, but packages and future harness code
-- need one durable place for block-shaped state.
CREATE TABLE IF NOT EXISTS context_blocks (
  id             INTEGER PRIMARY KEY,
  scope          TEXT NOT NULL,   -- global | agent | session | run
  owner          TEXT NOT NULL,   -- agent/human/package identity that owns the block
  session_id     TEXT,
  run_id         TEXT,
  name           TEXT NOT NULL,
  placement      TEXT NOT NULL,   -- system | before_messages | after_messages | user | scratch
  priority       INTEGER NOT NULL DEFAULT 0,
  package        TEXT,
  content        TEXT NOT NULL,
  content_sha256 TEXT NOT NULL,
  meta           TEXT,
  created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  UNIQUE(scope, owner, session_id, run_id, name)
);
CREATE INDEX IF NOT EXISTS idx_context_blocks_owner ON context_blocks(owner, scope, name);
CREATE INDEX IF NOT EXISTS idx_context_blocks_session ON context_blocks(session_id, name);

-- Durable context build log: enough to reconstruct which component added,
-- moved, rewrote, validated, or dropped blocks while building a provider
-- request. Store summaries/hashes, not full prompt documents.
CREATE TABLE IF NOT EXISTS context_build_log (
  id             INTEGER PRIMARY KEY,
  session_id     TEXT NOT NULL,
  run_id         TEXT,
  profile        TEXT NOT NULL,
  agent          TEXT NOT NULL,
  request_ordinal INTEGER,
  component      TEXT NOT NULL,
  action         TEXT NOT NULL,
  block_name     TEXT,
  before_sha256  TEXT,
  after_sha256   TEXT,
  summary        TEXT,
  meta           TEXT,
  created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_context_build_log_session ON context_build_log(session_id, id);
CREATE INDEX IF NOT EXISTS idx_context_build_log_run ON context_build_log(run_id, id);

-- Durable coding-session records (docs/handoffs/coding-agents.md, M2-A). One row
-- per launched coding session, mapping the elanus session id to the tool's own
-- NATIVE resumable session id (codex thread_id / Claude Code session_id), the
-- tool, the agent noun it publishes under, and the workdir it ran in. This is the
-- DURABLE half of the split session model: the record carries NO secret and
-- survives process exit, so an idle resumable session has a record but no live
-- credential. The ephemeral scoped TOKEN (src/codesession.rs) is minted per run
-- and per resume and retired at the end — the record is what `elanus code resume`
-- looks up to mint a fresh token and continue the native session in its workdir.
-- Written once the native id is known (codex: thread.started; CC: SessionStart),
-- updated on each resume (last_active). Keyed by the elanus session.
CREATE TABLE IF NOT EXISTS code_sessions (
  id             INTEGER PRIMARY KEY,
  elanus_session TEXT NOT NULL UNIQUE,  -- code-<8hex>, the elanus session id
  native_session TEXT NOT NULL,         -- codex thread_id / CC session_id (resume key)
  tool           TEXT NOT NULL,         -- the binary: claude | codex
  agent_noun     TEXT NOT NULL,         -- the obs noun: claude-code | codex
  workdir        TEXT NOT NULL,         -- absolute dir the session ran in (resume cwd)
  created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  last_active    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_code_sessions_native ON code_sessions(native_session);

-- Idempotency for driven coding-session deliveries (M4-A). Delivery is
-- at-least-once (docs/handoffs/coding-agents.md): a daemon crash mid-resume
-- re-pends the claimed event on the next start (boot's running->pending sweep),
-- which would drive a SECOND resume of an already-acted-on turn. Each delivery
-- carries a key — an explicit `idempotency_key` in the payload, else the inbound
-- event id — and is recorded here the moment it is claimed. A delivery whose key
-- is already present FOR THE SAME TARGET SESSION is recognized and settled as a
-- clean no-op (no second resume). The row is DURABLE, so the replay after a
-- restart is caught, not just a same-process duplicate.
--
-- The key is namespaced by the target `session` (PRIMARY KEY (session,
-- idempotency_key) — docs/security.md): a global key would let one principal
-- pre-claim an explicit key and silently SUPPRESS a different victim's delivery
-- to a DIFFERENT session that happens to reuse the same key (cross-victim
-- suppression). Namespacing by session means an explicit key only ever dedupes a
-- delivery to the SAME session, never collides across sessions. The default
-- `event:<id>` key is already globally unique (event ids are unique), so it is
-- unaffected; the replay dedupe (same delivery, same session, same key) still
-- holds across a restart.
CREATE TABLE IF NOT EXISTS code_delivery_keys (
  session         TEXT NOT NULL,     -- the coding session it was driven to
  idempotency_key TEXT NOT NULL,     -- explicit payload key, else "event:<id>"
  event_id        INTEGER,           -- the inbound delivery event id (audit)
  processed_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  PRIMARY KEY (session, idempotency_key)
);

-- Subagent lineage substrate. A subagent is an ordinary agent spawned by a
-- parent run; the launcher will insert one row per child session/run so
-- cancellation, budget attribution, and observability can follow the tree.
CREATE TABLE IF NOT EXISTS subagent_sessions (
  id                INTEGER PRIMARY KEY,
  parent_session_id TEXT NOT NULL,
  child_session_id  TEXT NOT NULL UNIQUE,
  parent_event_id   INTEGER REFERENCES events(id),
  parent_agent      TEXT NOT NULL,
  child_agent       TEXT NOT NULL,
  child_profile     TEXT NOT NULL,
  inherited_budget  INTEGER NOT NULL DEFAULT 1,
  context_program   TEXT,
  grant_policy      TEXT NOT NULL DEFAULT 'narrow',
  created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  cancelled_at      TEXT,
  finished_at       TEXT
);
CREATE INDEX IF NOT EXISTS idx_subagent_parent ON subagent_sessions(parent_session_id, id);
CREATE INDEX IF NOT EXISTS idx_subagent_child ON subagent_sessions(child_session_id);

-- A per-session memory note (M3): a small, editable block a planner leaves a
-- worker (or a persistent reminder), surfaced by the per-turn injection. One
-- row per session — the latest text wins (upsert). Deliberately minimal (a
-- stored string keyed by session); the full context_blocks substrate
-- integration is deferred (docs/handoffs/coding-agents.md M3 entry).
CREATE TABLE IF NOT EXISTS code_notes (
  session    TEXT PRIMARY KEY,   -- the coding session the note is for (code-<id>)
  note       TEXT NOT NULL,      -- the note text shown in the per-turn injection
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- Inbox read-tracking (M3): which of a session's own mailbox deliveries it has
-- already pulled via `elanus code inbox`. Keyed by (session, event_id) so the
-- read is idempotent — pulling twice does not re-surface the same message, and
-- the per-turn injection counts only UNSEEN deliveries. A session only ever
-- writes rows for ITS OWN deliveries (the inbox read is scoped to its own
-- env-derived mailbox by construction), so there is no cross-session write.
CREATE TABLE IF NOT EXISTS code_inbox_seen (
  session   TEXT NOT NULL,       -- the coding session that pulled the delivery
  event_id  INTEGER NOT NULL,    -- the delivery event id (the ledger row)
  seen_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  PRIMARY KEY (session, event_id)
);

-- Coordination-room membership (M5: advisory peer coordination,
-- docs/handoffs/coding-agents.md). Multiple concurrent coding sessions share a
-- room (`in/group/<id>`, ledger-backed — docs/topics.md); a session joins by a
-- row here (set at launch via `--room <id>`). This is the set a session shares
-- its claims with: a session SEES its roommates' claims (the point of the room)
-- and writes only its OWN. There is NO trust model — sessions are the user's own
-- cooperating agents (homogeneous authority); membership is conflict-avoidance
-- scope, not authorization.
--
-- Crash-released, mirroring the session-token reaper (src/codesession.rs): the
-- `owner_pid` is the launcher/driver pid that owns the live session, so a
-- SIGKILL'd session's membership (and its claims) are reaped at the next
-- launcher/daemon boot — a dead session's claims must not linger in roommates'
-- injections forever (the lease-released membership of docs/topics.md decided-5).
CREATE TABLE IF NOT EXISTS code_room_members (
  room       TEXT NOT NULL,       -- the room id (the <id> of in/group/<id>)
  session    TEXT NOT NULL,       -- the member coding session (code-<id>)
  agent_noun TEXT NOT NULL,       -- the obs noun it publishes under
  owner_pid  INTEGER NOT NULL,    -- the live owner pid; dead → reaped (crash-release)
  joined_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  PRIMARY KEY (room, session)
);
CREATE INDEX IF NOT EXISTS idx_code_room_members_session ON code_room_members(session);

-- Advisory edit claims (M5). A session announces "I'm editing <path>"; the
-- claim is recorded here, in the session's room, durably. It is ADVISORY
-- metadata, not a lock — recording one never blocks anyone; it surfaces in the
-- OTHER sessions' per-turn injection so they can route around it. A session can
-- record/clear only its OWN claims (its env-derived identity), but SEES its
-- roommates' (shared coordination). The raw path is stored verbatim in a column
-- (a path is a noun); `(room, session, path)` is the key so re-claiming the same
-- path is idempotent. Released with membership on session end / crash-reaped.
CREATE TABLE IF NOT EXISTS code_claims (
  room       TEXT NOT NULL,       -- the room the claim is visible in
  session    TEXT NOT NULL,       -- the session that holds the claim (code-<id>)
  path       TEXT NOT NULL,       -- the claimed path (raw, stored verbatim)
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  PRIMARY KEY (room, session, path)
);
CREATE INDEX IF NOT EXISTS idx_code_claims_room ON code_claims(room);
CREATE INDEX IF NOT EXISTS idx_code_claims_session ON code_claims(session);
"#,
    )?;
    // M5: the room a coding session belongs to, stored on the durable record so a
    // claim/resume can derive it from the session's own identity. Nullable —
    // pre-M5 records (and a session launched with no `--room`) have no room.
    let _ = conn.execute("ALTER TABLE code_sessions ADD COLUMN room TEXT", []);
    // Migrations for databases created before a column existed; the error on
    // a duplicate column is expected and ignored.
    let _ = conn.execute(
        "ALTER TABLE events ADD COLUMN emitted_by_dispatch INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE grants ADD COLUMN code_hash TEXT NOT NULL DEFAULT ''",
        [],
    );
    // DEFAULT 1: pre-existing rows count as already announced — upgrading
    // must not replay history onto the bus.
    let _ = conn.execute(
        "ALTER TABLE events ADD COLUMN announced INTEGER NOT NULL DEFAULT 1",
        [],
    );
    let _ = conn.execute("ALTER TABLE events ADD COLUMN sender TEXT", []);
    // Depends on the column above, so it lives after the migration, not in
    // the batch (a pre-`announced` DB would fail the whole batch otherwise).
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_events_unannounced ON events(announced) WHERE announced = 0",
        [],
    )?;
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
