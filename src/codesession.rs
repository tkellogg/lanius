//! Per-session coding-agent identity: a **grant-scoped** session actor token.
//!
//! A coding session (`elanus code launch claude …`) must publish to the bus as
//! *itself* (`sender = code-<session>`, never the owner — docs/actors.md egress
//! lesson / docs/security.md entry 16), but it must NOT carry owner-equivalent
//! authority. The earlier slice minted a plain fenced secret named
//! `code-<session>` in `Root::secrets()`; the broker resolves a fenced secret as
//! a **full-authority principal** (`actor = None`), which skips every bus ACL
//! gate — so a leaked session credential could publish to `in/human/owner`,
//! `work/agent/exec`, another agent's mailbox, and subscribe `obs/#`. That was
//! the high-severity authority gap.
//!
//! This module replaces that with a session token that the broker resolves as a
//! **grant-scoped actor** (`actor = Some(code-<session>)`), scoped *structurally*
//! to the one thing a coding session legitimately needs — publishing its own
//! `obs/agent/<agent>/<session>/#` telemetry — copying the grant-scoped shape of
//! the webhook daemon (carries its own token, narrow filter) rather than the
//! full-authority fenced-secret shape.
//!
//! ## Why a structural scope, not grant rows
//!
//! Package actors are grant-scoped via ledger rows pinned to a manifest hash
//! (src/packages.rs). A coding session has no manifest and no durable package —
//! it is ephemeral, one per launch. Rather than fabricate manifest/grant rows for
//! a transient principal, the scope is **derived from the session name**: a
//! `code-<session>` actor publishing `claude-code` telemetry is allowed exactly
//! `obs/agent/<agent>/<session>/#` and nothing else, and may subscribe only what
//! it is explicitly granted here (today: nothing). The scope is recorded in the
//! token file the launcher writes, so the broker reads one authoritative source.
//!
//! ## Why this is forge-resistant (the same asymmetry as before)
//!
//! The token store lives **inside the fenced secret store**
//! (`Root::secrets()/code-sessions/`), which the cage denies caged actors both
//! read and write (src/sandbox.rs `Protect::deny_all_trees`). Only the uncaged
//! launcher/kernel can place a token, so a caged agent can neither read an
//! existing session token nor forge a new session identity. Attribution stays
//! real (`sender = code-<session>`); only the *authority* changes — from
//! owner-equivalent to a narrow, structural scope.

use crate::paths::Root;
use crate::secrets;
use anyhow::{bail, Context, Result};
use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};

// ── The durable session RECORD (M2-A) ────────────────────────────────────────
//
// The split-session model (docs/handoffs/coding-agents.md) keeps the durable
// *record* of a session apart from the ephemeral *token* above. The record lives
// in `elanus.db` (`code_sessions`), carries **no secret**, and survives process
// exit: it maps the elanus session id to the tool's own native resumable session
// id (codex `thread_id` / CC `session_id`), the tool, the agent noun, and the
// workdir. An idle resumable session is exactly this — a record with no live
// token. `elanus code resume` reads the record to mint a FRESH scoped token and
// continue the native session in its recorded workdir, then retires the token.
// This preserves the verified "no idle live credential" property while enabling
// resume: the credential is per-run, the record is durable.

/// A durable coding-session record (the `code_sessions` row). Carries no secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// The elanus session id (`code-<8hex>`), the stable handle a human resumes.
    pub elanus_session: String,
    /// The tool's own native resumable session id — codex `thread_id` / CC
    /// `session_id`. This is what the native resume command targets.
    pub native_session: String,
    /// The binary that ran this session: `claude` | `codex`.
    pub tool: String,
    /// The obs agent noun the session publishes under: `claude-code` | `codex`.
    pub agent_noun: String,
    /// Absolute directory the session ran in; resume runs in the same dir so the
    /// native session continues against the same files.
    pub workdir: String,
    /// The coordination room (`in/group/<room>`) this session shares with its
    /// peers (M5), or None if it was launched without `--room`. A session sees
    /// its roommates' edit claims and writes only its own; this is the scope of
    /// that shared coordination — not a trust boundary.
    pub room: Option<String>,
}

/// Persist (or update) the durable record once the native session id is known
/// (codex: on `thread.started`; CC: on the SessionStart hook). Idempotent per
/// elanus session: a re-observed native id (e.g. a second SessionStart) refreshes
/// `native_session`/`workdir` and bumps `last_active` rather than duplicating.
/// Best-effort callers may ignore the error — a missing record just means that
/// session can't be resumed, never that the live session breaks.
pub fn upsert_record(root: &Root, rec: &SessionRecord) -> Result<()> {
    let conn = crate::db::open(root).context("opening the ledger for the session record")?;
    crate::db::init_schema(&conn)?;
    conn.execute(
        "INSERT INTO code_sessions
           (elanus_session, native_session, tool, agent_noun, workdir, room)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(elanus_session) DO UPDATE SET
           native_session = excluded.native_session,
           tool           = excluded.tool,
           agent_noun     = excluded.agent_noun,
           workdir        = excluded.workdir,
           -- Preserve a room set at launch when a later observation (e.g. a CC
           -- SessionStart that doesn't carry the room) re-upserts the record:
           -- only overwrite when the new value is non-null (COALESCE keeps the
           -- existing room rather than clearing it).
           room           = COALESCE(excluded.room, code_sessions.room),
           last_active    = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        rusqlite::params![
            rec.elanus_session,
            rec.native_session,
            rec.tool,
            rec.agent_noun,
            rec.workdir,
            rec.room,
        ],
    )?;
    Ok(())
}

/// Read a durable record by elanus session id. None if there is no such session
/// (never launched, or launched but the native id was never observed).
pub fn read_record(root: &Root, elanus_session: &str) -> Result<Option<SessionRecord>> {
    let conn = crate::db::open(root).context("opening the ledger for the session record")?;
    crate::db::init_schema(&conn)?;
    let rec = conn
        .query_row(
            "SELECT elanus_session, native_session, tool, agent_noun, workdir, room
             FROM code_sessions WHERE elanus_session = ?1",
            [elanus_session],
            |r| {
                Ok(SessionRecord {
                    elanus_session: r.get(0)?,
                    native_session: r.get(1)?,
                    tool: r.get(2)?,
                    agent_noun: r.get(3)?,
                    workdir: r.get(4)?,
                    room: r.get(5)?,
                })
            },
        )
        .ok();
    Ok(rec)
}

// ── Delivery idempotency (M4-A) ───────────────────────────────────────────────
//
// Driven deliveries are at-least-once (docs/handoffs/coding-agents.md): a daemon
// crash mid-resume re-pends the claimed event on the next start, which would
// otherwise drive a SECOND resume of an already-acted-on turn. Each delivery
// carries a key (an explicit payload `idempotency_key`, else the inbound event
// id); we record it DURABLY the moment the delivery is claimed, so the replay
// after a restart is recognized and skipped — not just a same-process duplicate.
//
// The key is namespaced by the TARGET SESSION (docs/security.md). A global key
// space let an attacker pre-claim an explicit key for one session and silently
// suppress a different victim's delivery to a DIFFERENT session that reused the
// key (cross-victim suppression). Keyed by `(session, key)`, an explicit key only
// dedupes a delivery to the SAME session — one principal's key can never collide
// with another principal's delivery to a different session. The default
// `event:<id>` key is globally unique regardless, so it is unaffected.

/// Record a delivery's idempotency key as processed FOR ITS TARGET SESSION.
/// Returns `true` if this is the FIRST time the key is seen for that session (the
/// delivery should be driven), `false` if it was already recorded (a duplicate —
/// the caller skips the resume as a clean no-op). Atomic via
/// `INSERT … ON CONFLICT DO NOTHING` on `(session, key)`, so two concurrent claims
/// of the same key+session cannot both win the race, while the same key for a
/// DIFFERENT session is a distinct row (no cross-victim suppression). Durable:
/// survives a restart, so the at-least-once replay is caught.
pub fn claim_delivery_key(
    root: &Root,
    key: &str,
    session: &str,
    event_id: i64,
) -> Result<bool> {
    let conn = crate::db::open(root).context("opening the ledger for the delivery key")?;
    crate::db::init_schema(&conn)?;
    let inserted = conn.execute(
        "INSERT INTO code_delivery_keys (session, idempotency_key, event_id)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(session, idempotency_key) DO NOTHING",
        rusqlite::params![session, key, event_id],
    )?;
    Ok(inserted == 1)
}

/// Has this delivery key already been processed FOR THIS SESSION? A read-only
/// check (the claim itself is `claim_delivery_key`). Scoped by session so a key
/// claimed against a different session does not read as seen here (the
/// cross-victim suppression the namespacing closes). Best-effort: a db error reads
/// as "not seen" so a transient failure never silently drops a real delivery.
pub fn delivery_key_seen(root: &Root, key: &str, session: &str) -> bool {
    let Ok(conn) = crate::db::open(root) else {
        return false;
    };
    conn.query_row(
        "SELECT 1 FROM code_delivery_keys WHERE session = ?1 AND idempotency_key = ?2",
        [session, key],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

// ── The session inbox + memory note (M3) ──────────────────────────────────────
//
// M3 gives a session its FIRST read capability — but a narrow, structural one
// that does NOT widen the emit-only bus token (which stays subscribe-empty). A
// session reads ONLY its own inbox, and it does so as a SCOPED LEDGER QUERY by
// its own identity, not over the bus: `inbox_for_session` selects the `events`
// rows whose topic is the session's own mailbox `in/agent/<noun>/<session>`. The
// caller (the `elanus code inbox` CLI) derives `<noun>`/`<session>` from the
// process env the launcher set (ELANUS_CODE_AGENT / ELANUS_CODE_SESSION), never
// from an argument — so a session can never name another session's inbox, and
// the read is own-inbox-only BY CONSTRUCTION. The bus token's subscribe scope is
// untouched (still empty): the read authority is the kernel-side query, gated by
// the env-derived identity, exactly as `elanus code hook` publishes as itself.

/// One delivery in a session's own inbox (a `code-*` mailbox `events` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxItem {
    /// The ledger event id (the durable delivery row).
    pub event_id: i64,
    /// The message text the delivery carried (the `prompt`/`text` field).
    pub message: String,
    /// The broker-verified sender of the delivery (who it is from), if recorded.
    pub from: Option<String>,
    /// The correlation that threads this delivery's round trip, if any.
    pub correlation: Option<String>,
    /// The delivery's lifecycle state (pending / running / done / failed …) —
    /// honest about whether the daemon has already driven it.
    pub state: String,
    /// When the delivery was recorded.
    pub created_at: String,
    /// Whether this session has already pulled this delivery via `code inbox`.
    pub seen: bool,
}

/// Read a session's OWN inbox: the deliveries on ITS mailbox topic
/// `in/agent/<noun>/<session>`. `noun` and `session` MUST come from the running
/// session's own env (the CLI derives them), never from a caller-supplied id —
/// that is what makes this own-inbox-only by construction. With `unseen_only`,
/// returns just the deliveries this session has not yet pulled (the per-turn
/// status counts these); otherwise the full inbox. Newest last (chronological).
/// The mailbox topic is built with `encode_segment` so it matches exactly what
/// the launcher/deliverer addressed (the same encoding `recognize_delivery` and
/// `record_delivery` use), even for a name with reserved characters.
pub fn inbox_for_session(
    root: &Root,
    agent_noun: &str,
    session: &str,
    unseen_only: bool,
) -> Result<Vec<InboxItem>> {
    // Guard: only a real `code-*` session has an inbox, and the mailbox is built
    // from the session's own identity. A non-session name yields nothing rather
    // than a crafted topic.
    if !is_session_principal(session) {
        return Ok(Vec::new());
    }
    let mailbox = format!(
        "in/agent/{}/{}",
        crate::topic::encode_segment(agent_noun),
        crate::topic::encode_segment(session),
    );
    let conn = crate::db::open(root).context("opening the ledger for the inbox")?;
    crate::db::init_schema(&conn)?;
    // The session's own mailbox rows, joined to its seen-set. The `session`
    // binding on the LEFT JOIN is the SAME env-derived session, so a row's seen
    // flag is THIS session's read state, never another's.
    let mut stmt = conn.prepare(
        "SELECT e.id, COALESCE(e.payload,''), e.sender, e.correlation_id, e.state, e.created_at,
                (s.event_id IS NOT NULL) AS seen
         FROM events e
         LEFT JOIN code_inbox_seen s
           ON s.session = ?1 AND s.event_id = e.id
         WHERE e.type = ?2
         ORDER BY e.id ASC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![session, mailbox], |r| {
            let payload: String = r.get(1)?;
            let seen: bool = r.get(6)?;
            Ok((
                r.get::<_, i64>(0)?,
                payload,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                seen,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for (event_id, payload, from, correlation, state, created_at, seen) in rows {
        if unseen_only && seen {
            continue;
        }
        let pv: serde_json::Value = serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
        // Reuse the same message extraction the daemon uses to drive a delivery,
        // so the inbox shows exactly what a resume would act on.
        let message = crate::codeagent::delivery_message(&pv).unwrap_or_default();
        items.push(InboxItem {
            event_id,
            message,
            from,
            correlation,
            state,
            created_at,
            seen,
        });
    }
    Ok(items)
}

/// Mark a set of the session's own inbox deliveries as seen (idempotent). Called
/// after `elanus code inbox` lists them, so a second pull does not re-surface the
/// same messages and the per-turn count reflects only genuinely new deliveries.
/// Writes ONLY rows for the env-derived `session` — a session can never mark
/// another session's deliveries seen. `INSERT … ON CONFLICT DO NOTHING` so a
/// re-mark is a no-op.
pub fn mark_inbox_seen(root: &Root, session: &str, event_ids: &[i64]) -> Result<()> {
    if event_ids.is_empty() || !is_session_principal(session) {
        return Ok(());
    }
    let conn = crate::db::open(root).context("opening the ledger to mark the inbox seen")?;
    crate::db::init_schema(&conn)?;
    for id in event_ids {
        conn.execute(
            "INSERT INTO code_inbox_seen (session, event_id) VALUES (?1, ?2)
             ON CONFLICT(session, event_id) DO NOTHING",
            rusqlite::params![session, id],
        )?;
    }
    Ok(())
}

/// Set (or replace) a session's memory note — the small editable block a planner
/// leaves a worker, surfaced by the per-turn injection. One row per session; the
/// latest text wins. An empty note clears it (a deliberate way to remove a stale
/// reminder). The session must be a valid `code-*` id.
pub fn set_note(root: &Root, session: &str, note: &str) -> Result<()> {
    if !is_session_principal(session) {
        bail!("note session {session:?} is not a valid code-* identity name");
    }
    let conn = crate::db::open(root).context("opening the ledger to set the note")?;
    crate::db::init_schema(&conn)?;
    let note = note.trim();
    if note.is_empty() {
        conn.execute("DELETE FROM code_notes WHERE session = ?1", [session])?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO code_notes (session, note) VALUES (?1, ?2)
         ON CONFLICT(session) DO UPDATE SET
           note = excluded.note,
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        rusqlite::params![session, note],
    )?;
    Ok(())
}

/// Read a session's memory note, if one is set. None when there is no note (the
/// per-turn injection omits the note line in that case).
pub fn get_note(root: &Root, session: &str) -> Result<Option<String>> {
    if !is_session_principal(session) {
        return Ok(None);
    }
    let conn = crate::db::open(root).context("opening the ledger to read the note")?;
    crate::db::init_schema(&conn)?;
    let note = conn
        .query_row(
            "SELECT note FROM code_notes WHERE session = ?1",
            [session],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(note)
}

// ── M5: coordination room membership + advisory edit claims ───────────────────
//
// Advisory peer coordination (docs/handoffs/coding-agents.md M5): multiple
// concurrent coding sessions share a ROOM; each announces edit CLAIMS ("I'm
// editing src/foo.rs"); each session's per-turn injection (M3) surfaces its
// ROOMMATES' current claims (excluding its own) so cooperating workers route
// around each other. This is conflict-avoidance, NOT authorization — there is no
// trust boundary between the user's own agents, nothing is locked or gated. A
// claim is advisory metadata the others read.
//
// The scope discipline mirrors M3's inbox: a session reads its ROOM's claims (the
// sessions it shares a room with), and writes/clears only its OWN (its env-derived
// identity), exactly as `code inbox` reads only its own mailbox and `code hook`
// publishes as itself. The room a session belongs to is on its durable record
// (set at launch), so `claim`/`unclaim` derive BOTH the session (from env) and the
// room (from the record) — a session can never name another session's claim or a
// room it isn't in.
//
// Crash-released, mirroring `reap_orphans` for the session token: membership
// carries the owner pid, so a SIGKILL'd session's membership and claims are reaped
// at the next launcher/daemon boot (a dead session's claims must not linger in its
// roommates' injections forever — the lease-released membership of docs/topics.md
// decided-5).

/// One advisory edit claim visible in a room: a peer session is working on a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The session that holds the claim (`code-<id>`) — who is editing.
    pub session: String,
    /// The path the session claims to be working on (raw, as recorded).
    pub path: String,
    /// When the claim was recorded.
    pub created_at: String,
}

/// Set the room on a session's durable record without disturbing the rest of it.
/// The launcher calls this at launch (before the native session id is observed),
/// so a later `upsert_record` for the native id — which carries `room: None` —
/// preserves it via the COALESCE in `upsert_record`. Creates a stub record if the
/// native id isn't known yet (it will be refreshed on `thread.started` /
/// SessionStart). Best-effort callers may ignore the error.
pub fn set_room(root: &Root, elanus_session: &str, room: &str) -> Result<()> {
    let conn = crate::db::open(root).context("opening the ledger to set the room")?;
    crate::db::init_schema(&conn)?;
    // Update an existing record's room; if there is none yet (the common case at
    // launch, before the native id), the room is carried on the membership row and
    // applied to the record when the native-id upsert runs (COALESCE preserves a
    // room already present). To keep the record the single source of truth for the
    // room a resume reads, we upsert a row carrying just the room — the native_id
    // upsert later fills the rest.
    let n = conn.execute(
        "UPDATE code_sessions SET room = ?2 WHERE elanus_session = ?1",
        rusqlite::params![elanus_session, room],
    )?;
    if n == 0 {
        // No record yet: create a stub the native-id upsert will complete. The
        // placeholder native_session/tool/agent_noun are overwritten on the first
        // real upsert (keyed by elanus_session); the room is preserved by COALESCE.
        conn.execute(
            "INSERT INTO code_sessions
               (elanus_session, native_session, tool, agent_noun, workdir, room)
             VALUES (?1, '', '', '', '', ?2)
             ON CONFLICT(elanus_session) DO UPDATE SET room = excluded.room",
            rusqlite::params![elanus_session, room],
        )?;
    }
    Ok(())
}

/// Record a session's room membership (join). Idempotent per `(room, session)`;
/// re-joining refreshes the owner pid (so a re-launched/re-driven session updates
/// the liveness pid). The owner pid is the live process that owns the session, so
/// the reaper can release a SIGKILL'd session's membership. A session is only ever
/// a member of ONE room here (the room on its record); joining a different room is
/// a fresh row, but the launcher only ever joins the one room it was given.
pub fn join_room(
    root: &Root,
    room: &str,
    session: &str,
    agent_noun: &str,
    owner_pid: i32,
) -> Result<()> {
    if !is_session_principal(session) {
        bail!("room member {session:?} is not a valid code-* identity name");
    }
    let conn = crate::db::open(root).context("opening the ledger to join the room")?;
    crate::db::init_schema(&conn)?;
    conn.execute(
        "INSERT INTO code_room_members (room, session, agent_noun, owner_pid)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(room, session) DO UPDATE SET
           agent_noun = excluded.agent_noun,
           owner_pid  = excluded.owner_pid",
        rusqlite::params![room, session, agent_noun, owner_pid],
    )?;
    Ok(())
}

/// Record an advisory edit claim: `session` is working on `path` in `room`. The
/// caller derives `room`/`session` from the session's OWN identity (env-derived
/// session + the room on its record), never from a peer-supplied argument — so a
/// session can only ever claim as itself, in its own room. Idempotent per
/// `(room, session, path)` (re-claiming the same path refreshes the timestamp).
/// Recording a claim NEVER blocks anyone — it is advisory metadata, not a lock.
/// The path is stored verbatim (a path is a noun).
pub fn add_claim(root: &Root, room: &str, session: &str, path: &str) -> Result<()> {
    if !is_session_principal(session) {
        bail!("claim holder {session:?} is not a valid code-* identity name");
    }
    let path = path.trim();
    if path.is_empty() {
        bail!("a claim path must not be empty");
    }
    let conn = crate::db::open(root).context("opening the ledger to record the claim")?;
    crate::db::init_schema(&conn)?;
    conn.execute(
        "INSERT INTO code_claims (room, session, path) VALUES (?1, ?2, ?3)
         ON CONFLICT(room, session, path) DO UPDATE SET
           created_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        rusqlite::params![room, session, path],
    )?;
    Ok(())
}

/// Clear one of a session's OWN advisory claims (unclaim a path it finished). Only
/// the holder's own `(room, session, path)` row is removed — a session can never
/// clear a peer's claim (the room/session are its own env-derived identity).
/// Idempotent: unclaiming a path it doesn't hold is a no-op. Returns whether a row
/// was removed (so the CLI can report honestly).
pub fn remove_claim(root: &Root, room: &str, session: &str, path: &str) -> Result<bool> {
    let path = path.trim();
    let conn = crate::db::open(root).context("opening the ledger to clear the claim")?;
    crate::db::init_schema(&conn)?;
    let n = conn.execute(
        "DELETE FROM code_claims WHERE room = ?1 AND session = ?2 AND path = ?3",
        rusqlite::params![room, session, path],
    )?;
    Ok(n > 0)
}

/// List the claims a session should SEE in its room: every claim in `room` held by
/// a session OTHER than `viewer` (its peers' claims — its own are excluded, that
/// is the point: a worker sees what its roommates are touching). Newest last. Used
/// by the M3 per-turn injection to surface "peers: code-X is editing src/foo.rs".
/// When `room` is empty/None at the call site, the caller passes nothing and gets
/// no peer claims (a session with no room has no peers).
pub fn peer_claims(root: &Root, room: &str, viewer: &str) -> Result<Vec<Claim>> {
    if room.is_empty() {
        return Ok(Vec::new());
    }
    let conn = crate::db::open(root).context("opening the ledger to read peer claims")?;
    crate::db::init_schema(&conn)?;
    let mut stmt = conn.prepare(
        "SELECT session, path, created_at FROM code_claims
         WHERE room = ?1 AND session <> ?2
         ORDER BY created_at ASC, session ASC, path ASC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![room, viewer], |r| {
            Ok(Claim {
                session: r.get(0)?,
                path: r.get(1)?,
                created_at: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// List a session's OWN current claims in a room (what it has announced). For the
/// `claim`/`unclaim` CLI to show the holder its own state. Newest last.
pub fn own_claims(root: &Root, room: &str, session: &str) -> Result<Vec<Claim>> {
    if room.is_empty() {
        return Ok(Vec::new());
    }
    let conn = crate::db::open(root).context("opening the ledger to read own claims")?;
    crate::db::init_schema(&conn)?;
    let mut stmt = conn.prepare(
        "SELECT session, path, created_at FROM code_claims
         WHERE room = ?1 AND session = ?2
         ORDER BY created_at ASC, path ASC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![room, session], |r| {
            Ok(Claim {
                session: r.get(0)?,
                path: r.get(1)?,
                created_at: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Reap room memberships (and their claims) whose owning session process is dead —
/// a SIGKILL'd session never ran its clean `release_session`, so its claims would
/// otherwise linger in roommates' injections forever. Mirrors `reap_orphans` for
/// the session token: signal-0 liveness probe on the recorded `owner_pid`, treat
/// EPERM as alive (a cross-uid live session is never wrongly reaped). Run at daemon
/// boot and launcher boot, crash-only like every other liveness sweep. Returns the
/// `(room, session)` pairs reaped.
pub fn reap_dead_members(root: &Root) -> Vec<(String, String)> {
    let mut reaped = Vec::new();
    let Ok(conn) = crate::db::open(root) else {
        return reaped;
    };
    if crate::db::init_schema(&conn).is_err() {
        return reaped;
    }
    let members: Vec<(String, String, i32)> = {
        let Ok(mut stmt) =
            conn.prepare("SELECT room, session, owner_pid FROM code_room_members")
        else {
            return reaped;
        };
        let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i32>(2)?,
            ))
        }) else {
            return reaped;
        };
        rows.filter_map(|r| r.ok()).collect()
    };
    for (room, session, owner_pid) in members {
        if !pid_alive(owner_pid) {
            let _ = conn.execute(
                "DELETE FROM code_claims WHERE room = ?1 AND session = ?2",
                rusqlite::params![room, session],
            );
            let _ = conn.execute(
                "DELETE FROM code_room_members WHERE room = ?1 AND session = ?2",
                rusqlite::params![room, session],
            );
            reaped.push((room, session));
        }
    }
    reaped
}

/// Bump a record's `last_active` to now (after a resume completes). Best-effort.
pub fn touch_record(root: &Root, elanus_session: &str) -> Result<()> {
    let conn = crate::db::open(root)?;
    conn.execute(
        "UPDATE code_sessions SET last_active = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE elanus_session = ?1",
        [elanus_session],
    )?;
    Ok(())
}

/// The session-id prefix that marks a coding-session actor everywhere (CONNECT
/// resolution, ACL, reaping). A principal name starting with this is resolved
/// through this module, never the full-authority fenced-secret path.
pub const PREFIX: &str = "code-";

/// Every harness-controlled authority dimension for a session principal, unified
/// into one value. Carried on `SessionToken` via `#[serde(flatten)]` so the
/// on-disk JSON shape is UNCHANGED — tokens written by M1 (flat
/// `publish`/`subscribe`/`turn_budget`/`remaining_budget` fields) still
/// deserialize byte-for-byte (docs/handoffs/authority-delegation.md M2).
///
/// ## Dimensions
///
/// - **Bus capability (non-fungible):** `publish` and `subscribe` are MQTT-filter
///   vecs — the broker gates every publish/subscribe against these.
///   `child ⊆ spawner` (every child filter must be `covers`-ed by some spawner
///   filter) is asserted at mint (docs/security.md entry 22).
/// - **Budget (fungible):** `turn_budget` / `remaining_budget` — the M1 dimension;
///   `Σ children ≤ parent.remaining` is asserted at mint.
///
/// M3 will add fs-read/write and tool-allowlist dimensions here; each will carry
/// its own `⊆`/`Σ≤` assertion at mint and no changes to the existing fields.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Grants {
    /// Publish filters this session may publish to (structural: its own obs
    /// subtree). Everything else is denied by the broker ACL.
    #[serde(default)]
    pub publish: Vec<String>,
    /// Subscribe filters this session may subscribe to. Empty today — a coding
    /// session needs to *emit* its record, not read the bus, so it gets no read
    /// authority at all. A later explicit grant can widen this while still
    /// satisfying `child ⊆ spawner`.
    #[serde(default)]
    pub subscribe: Vec<String>,
    /// Total turn budget granted to this session. `None` = unbounded (the
    /// owner-spawned / common case). Set at mint; decremented into
    /// `remaining_budget` as children are allocated (M1). Absent in tokens
    /// written before M1 → deserializes as `None`.
    #[serde(default)]
    pub turn_budget: Option<u64>,
    /// Remaining turns this session may still allocate to children. Starts
    /// equal to `turn_budget`; the spawner's persisted token is rewritten
    /// (remaining decremented) each time a child is minted. `None` = unbounded
    /// (no cap). Absent in tokens written before M1 → deserializes as `None`.
    #[serde(default)]
    pub remaining_budget: Option<u64>,
}

/// One minted session token plus the structural scope the broker enforces for
/// it. Stored as JSON at `<root>/.secrets/code-sessions/<session>.json`.
///
/// ## On-disk JSON shape (back-compat invariant)
///
/// `grants` is stored with `#[serde(flatten)]`, so the serialized form is
/// identical to M1's flat layout — `publish`, `subscribe`, `turn_budget`, and
/// `remaining_budget` appear at the top level, not nested under a `grants` key.
/// M1-era tokens deserialize without modification; `Grants` fields default to
/// their natural zero (empty vecs / `None`) when absent.
///
/// ## Budget dimension (M1 — docs/handoffs/authority-delegation.md)
///
/// `grants.turn_budget` is the fungible authority dimension. `grants.remaining_budget`
/// is the runtime balance. See `Grants` for the full description and the
/// `flock`/atomic-write discipline.
///
/// ## Bus-scope dimension (M2 — docs/handoffs/authority-delegation.md)
///
/// `grants.publish` / `grants.subscribe` are the bus capability dimensions.
/// `child ⊆ spawner` is asserted at mint via `topic::covers`. See `mint` and
/// `Grants` for details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionToken {
    /// The session principal, e.g. `code-2af51b7e`. Equals the file stem.
    pub principal: String,
    /// The agent noun this session publishes under (`claude-code`, `codex`).
    pub agent: String,
    /// The secret the child presents as ELANUS_BUS_TOKEN.
    pub secret: String,
    /// The launcher pid that owns this session — used by the reaper to tell a
    /// live session's token from an orphan a SIGKILL left behind.
    pub owner_pid: i32,
    /// All harness-controlled authority dimensions for this session. Stored
    /// flattened so the on-disk JSON is unchanged from M1 (back-compat).
    #[serde(flatten)]
    pub grants: Grants,
}

impl SessionToken {
    /// May this session publish to `topic_name`? Delegates to the grants.
    pub fn may_publish(&self, topic_name: &str) -> bool {
        self.grants
            .publish
            .iter()
            .any(|f| crate::topic::matches(f, topic_name))
    }
    /// May this session subscribe to `filter`? Exact-filter match against the
    /// granted set (today: none for the structural default).
    pub fn may_subscribe(&self, filter: &str) -> bool {
        self.grants.subscribe.iter().any(|f| f == filter)
    }
}

/// Is this principal a coding-session actor (resolved through this module)?
pub fn is_session_principal(name: &str) -> bool {
    name.starts_with(PREFIX) && secrets::valid_principal(name)
}

/// The token store directory, inside the fenced secret store so the cage denies
/// caged actors read+write (the forge-resistance asymmetry).
fn store_dir(root: &Root) -> PathBuf {
    root.secrets().join("code-sessions")
}

fn token_path(root: &Root, principal: &str) -> PathBuf {
    store_dir(root).join(format!("{principal}.json"))
}

/// Path of the cross-process advisory lock file for the budget critical section.
fn budget_lock_path(root: &Root) -> PathBuf {
    store_dir(root).join("budget.lock")
}

/// RAII guard that holds an exclusive `flock(LOCK_EX)` on the budget lock file.
///
/// Acquiring: `BudgetLock::acquire(root)` opens (or creates) the lock file and
/// calls `flock(LOCK_EX)`, blocking until the lock is available. Because `flock`
/// is scoped to the open-file-description (not the fd number), a new open per
/// acquisition correctly serializes both:
/// - Separate OS processes that each call `mint()` in parallel (the fan-out case).
/// - Separate threads within one process that each open their own fd.
///
/// Releasing: `drop(BudgetLock)` calls `flock(LOCK_UN)` then closes the file.
/// The file itself is never removed — it is a stable sentinel; its presence is
/// harmless and removing it would race with concurrent acquirers.
#[cfg(unix)]
struct BudgetLock {
    fd: std::os::unix::io::RawFd,
}

#[cfg(unix)]
impl BudgetLock {
    fn acquire(root: &Root) -> std::io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = budget_lock_path(root);
        // create_dir_all is idempotent; the store dir may already exist.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false) // lock file: content is irrelevant, do not truncate
            .mode(0o600)
            .open(&path)?;
        let fd = std::os::unix::io::IntoRawFd::into_raw_fd(file);
        // LOCK_EX | LOCK_NB would be non-blocking; we want blocking (LOCK_EX only).
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(err);
        }
        Ok(BudgetLock { fd })
    }
}

#[cfg(unix)]
impl Drop for BudgetLock {
    fn drop(&mut self) {
        // flock(LOCK_UN) releases the lock; close() releases the fd. Both are
        // best-effort in a destructor — errors here cannot be surfaced.
        unsafe {
            libc::flock(self.fd, libc::LOCK_UN);
            libc::close(self.fd);
        }
    }
}

/// Mint a grant-scoped session token for `principal` publishing `agent`
/// telemetry. Writes the 0600 token file inside the fenced store and returns
/// the token (the launcher hands `.secret` to the child as ELANUS_BUS_TOKEN).
/// The default scope is structural: publish only `obs/agent/<agent>/<session>/#`,
/// subscribe nothing.
///
/// ## Budget dimension (M1 — docs/handoffs/authority-delegation.md, security.md entry 22)
///
/// `spawner` is the session that is minting this child (read from the fenced
/// token store — **never** from an inherited env variable, which is the
/// authority the doctrine forbids reconstructing from). Pass `None` when the
/// spawner is the owner (no session token exists → unbounded) or when the
/// spawning context is already the top of the chain.
///
/// `requested_budget` is how many turns the child requests. `None` = inherit
/// the spawner's full remaining (the **inherit-equal** default — narrowing
/// only happens on an explicit request). Explicit values must be ≤ the
/// spawner's remaining; if they would over-allocate, `mint` returns an error
/// and the spawn is refused — the same decidable, boring check sandbox.md
/// demands of `lease ⊆ grant`.
///
/// The invariant enforced here: **Σ children ≤ parent.remaining** (budget is
/// fungible — siblings partition, not share, the parent's allocation). When
/// the spawner token file exists on disk, `mint` always acquires an exclusive
/// `flock(LOCK_EX)` on `<store>/budget.lock` before reading the spawner token,
/// classifying it (bounded vs. unbounded), and writing back the decremented
/// value. The boundary signal is the FILE'S EXISTENCE, not a parse result —
/// this prevents a torn read (empty/partial file during a concurrent write)
/// from being misclassified as "no token → unbounded". Token writes use atomic
/// `rename(2)` so readers always see a complete file, eliminating the
/// truncate→write window. An unreadable spawner token inside the lock is
/// refused (fail-closed), never treated as unlimited authority.
///
/// Owner path (spawner = `None`) and absent-file path (pre-M1 tokens, or no
/// spawner session): the budget check is vacuously satisfied. The lock is NOT
/// acquired on these paths — they remain zero-overhead and zero-behavior-change
/// for all existing call sites.
///
/// ## Bus-scope dimension (M2 — docs/handoffs/authority-delegation.md, security.md entry 22)
///
/// `requested_publish` / `requested_subscribe` allow the caller to request a
/// narrower-than-default bus scope for the child. `None` = use the structural
/// default (`publish`: own obs subtree; `subscribe`: empty). An explicit
/// request that would widen beyond the spawner's grants is refused at mint.
///
/// When the spawner IS a finite-scope session (token file exists), the child's
/// bus grants are bounded by the spawner's under the same `flock(LOCK_EX)` that
/// serializes the budget critical section:
///
/// - **subscribe (read authority):** every child subscribe filter must be
///   `covers`-ed by some spawner subscribe filter (`child.subscribe ⊆
///   spawner.subscribe`). Today sessions get empty subscribe, so the default
///   child subscribe (also empty) trivially satisfies this; this is the
///   forward-looking guard.
///
/// - **publish:** the child may ALWAYS emit its OWN structural self-telemetry
///   subtree (`obs/agent/<agent>/<session>/#`) — this is its own audit trail
///   and NOT a widening of authority. Beyond that, every publish filter must
///   be `covers`-ed by some spawner publish filter (or by the child's own
///   structural subtree). The DEFAULT child.publish is exactly its own subtree,
///   so it always passes the check.
///
/// When the spawner is `None` (owner/top-of-chain), the child gets its
/// structural default unconditionally — zero behavior change for the common case.
// TODO M4: bundle requested_budget/requested_publish/requested_subscribe into a
// single `RequestedGrants` struct when the `--grants` CLI surface lands — that is
// the natural place to unify the request side the way `Grants` unifies the held
// side. Until then the explicit params keep the call sites legible.
#[allow(clippy::too_many_arguments)]
pub fn mint(
    root: &Root,
    principal: &str,
    agent: &str,
    owner_pid: i32,
    spawner: Option<&str>,
    requested_budget: Option<u64>,
    requested_publish: Option<Vec<String>>,
    requested_subscribe: Option<Vec<String>>,
) -> Result<SessionToken> {
    if !is_session_principal(principal) {
        bail!("session principal {principal:?} is not a valid code-* identity name");
    }
    let dir = store_dir(root);
    std::fs::create_dir_all(&dir)?;
    // 0700 the store dir — defense in depth on top of the cage fence over the
    // whole .secrets tree (the parent is already 0700, but match its posture).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }

    // ── Budget invariant: Σ children ≤ parent.remaining ─────────────────────
    //
    // Read the spawner's token from the fenced store (the authoritative source).
    // NEVER read from the environment — the doctrine (security.md entry 22) is
    // explicit: authority is reconstructed at spawn from the persisted record,
    // not blindly inherited. If the spawner has no token (owner path, or a
    // context that predates M1), its budget is unbounded and the check passes
    // vacuously.
    //
    // CROSS-PROCESS SERIALIZATION: whenever the spawner token file EXISTS on
    // disk, the entire read→check→decrement→write-back is serialized by an
    // exclusive advisory flock on `<store>/budget.lock`. Spawned workers are
    // detached OS processes (codeagent::launch calls cmd.spawn() without wait),
    // so a parent fanning out N children runs N parallel mint() calls — without
    // the lock, two siblings would both read the same parent_remaining, both
    // pass the per-child check, and Σ would silently exceed the parent.
    //
    // FAIL-CLOSED CLASSIFICATION: the decision of whether to take the lock-free
    // path is made by checking whether the spawner token FILE EXISTS on disk
    // (a stable POSIX signal), NOT by attempting to parse the file (which could
    // return None on a torn read and be misclassified as "unbounded"). If the
    // file exists but is unparseable inside the lock, the spawn is REFUSED —
    // a corrupt or half-written spawner token must never be treated as unlimited
    // authority.
    //
    // ATOMIC WRITES: write_0600 uses rename(2) so readers always see a complete
    // token, eliminating the truncate→write torn-read window that previously
    // existed. The exists-check and the locked read are belt-and-suspenders:
    // either fix alone closes the race; both together make it impossible.
    //
    // The lock-free fast paths:
    //   - spawner=None (owner/top-of-chain): no spawner file to check at all.
    //   - spawner file genuinely ABSENT (token predates M1, or owner context):
    //     unbounded → no lock needed. This preserves zero-overhead / zero-
    //     behavior-change for all existing call sites.
    //
    // NOTE: ELANUS_CODE_REPLY_TO is used in codeagent.rs to choose which spawner
    // token to charge. Pinning the spawner-name via a capability reference rather
    // than an env variable is a known follow-up (M2+). // TODO M2: replace env-key
    // spawner lookup with a capability-reference in the minted child token.
    // Structural scope: exactly the session's own obs subtree, encoded the same
    // way codeagent::obs_topic encodes the agent/session segments so the filter
    // and the published topics agree even for names with reserved characters.
    // This is the child's UNCONDITIONAL self-telemetry — its own audit trail —
    // and is never a widening of authority relative to the spawner (it is own-
    // data-only, structurally disjoint from everything else).
    let own_obs = format!(
        "obs/agent/{}/{}/#",
        crate::topic::encode_segment(agent),
        crate::topic::encode_segment(principal),
    );

    // ── Budget + bus-scope invariants: serialized by the same lock ───────────
    //
    // When the spawner token file EXISTS on disk, all reads/checks/writes for
    // BOTH the budget AND bus-scope dimensions are serialized under the same
    // exclusive flock(LOCK_EX). This is the same discipline M1 established;
    // M2 piggybacks on it to check child.publish ⊆ spawner.publish and
    // child.subscribe ⊆ spawner.subscribe.
    //
    // Lock-free fast paths (zero behavior change for the common case):
    //   - spawner=None (owner/top-of-chain): no spawner file to check.
    //   - spawner file genuinely ABSENT (pre-M1 token, or owner context):
    //     unbounded on all dimensions; no lock needed.
    let (child_budget, child_publish, child_subscribe) = if let Some(spawner_name) = spawner {
        let spawner_token_path = token_path(root, spawner_name);
        // Use the file's EXISTENCE as the branch signal (M1's fail-closed
        // discipline: see the M1 comment above for the full rationale).
        let file_exists = spawner_token_path.try_exists().unwrap_or(true);
        if !file_exists {
            // Spawner token file is genuinely absent (owner context or pre-M1
            // session): treat as unbounded on all dimensions.
            let pub_vec = requested_publish.unwrap_or_else(|| vec![own_obs.clone()]);
            let sub_vec = requested_subscribe.unwrap_or_default();
            (requested_budget, pub_vec, sub_vec)
        } else {
            // Spawner token file EXISTS → acquire the lock before reading,
            // checking, and writing back. Covers budget + bus-scope together.
            #[cfg(unix)]
            let _lock = BudgetLock::acquire(root)
                .map_err(|e| anyhow::anyhow!("budget lock acquire failed: {e}"))?;

            // Authoritative read under the lock.
            let spawner_tok = read(root, spawner_name).ok_or_else(|| {
                anyhow::anyhow!(
                    "budget spawn refused: spawner token {spawner_name:?} exists on disk \
                     but could not be parsed — treating as corrupt rather than unbounded \
                     (fail-closed: docs/security.md entry 22)"
                )
            })?;

            // ── Bus-scope: child ⊆ spawner ─────────────────────────────────
            //
            // The child's structural own-obs subtree is ALWAYS allowed — it is
            // the session's own audit trail and NOT a widening of the spawner's
            // authority. Every OTHER publish filter the child requests must be
            // covered by some spawner publish filter.
            //
            // subscribe: every child filter must be covered by some spawner
            // subscribe filter. Today spawner.subscribe is empty and the default
            // child.subscribe is empty (trivially satisfies ⊆).
            let child_pub_req = requested_publish.unwrap_or_else(|| vec![own_obs.clone()]);
            let child_sub_req = requested_subscribe.unwrap_or_default();

            // Check publish: child filter must be covered by own_obs OR some spawner pub filter.
            for filter in &child_pub_req {
                let covered_by_own = crate::topic::covers(&own_obs, filter);
                let covered_by_spawner = spawner_tok
                    .grants
                    .publish
                    .iter()
                    .any(|sf| crate::topic::covers(sf, filter));
                if !covered_by_own && !covered_by_spawner {
                    bail!(
                        "bus publish refused (docs/security.md entry 22): child filter \
                         {filter:?} is not covered by spawner {spawner_name:?}'s publish \
                         grants or its own structural subtree — child ⊆ spawner violated"
                    );
                }
            }

            // Check subscribe: child filter must be covered by some spawner sub filter.
            for filter in &child_sub_req {
                let covered = spawner_tok
                    .grants
                    .subscribe
                    .iter()
                    .any(|sf| crate::topic::covers(sf, filter));
                if !covered {
                    bail!(
                        "bus subscribe refused (docs/security.md entry 22): child filter \
                         {filter:?} is not covered by spawner {spawner_name:?}'s subscribe \
                         grants — child ⊆ spawner violated"
                    );
                }
            }

            // ── Budget: Σ children ≤ parent.remaining ──────────────────────
            let child_budget = match spawner_tok.grants.remaining_budget {
                None => {
                    // Token present and parseable but no budget cap (pre-M1 or
                    // owner-path token): treat as unbounded — no decrement.
                    requested_budget
                }
                Some(parent_remaining) => {
                    let child_alloc = requested_budget.unwrap_or(parent_remaining);
                    if child_alloc > parent_remaining {
                        bail!(
                            "budget allocation refused: child requested {child_alloc} turns but \
                             spawner {spawner_name:?} only has {parent_remaining} remaining \
                             (Σ children ≤ parent.remaining — docs/security.md entry 22)"
                        );
                    }
                    // Decrement the spawner's remaining budget and persist it
                    // BEFORE the lock is released and BEFORE the child token is
                    // written (fail-closed: see M1 comment above).
                    let mut tok = spawner_tok;
                    tok.grants.remaining_budget =
                        Some(parent_remaining.saturating_sub(child_alloc));
                    let json = serde_json::to_string(&tok).map_err(|e| {
                        anyhow::anyhow!(
                            "budget write-back serialization failed for spawner \
                             {spawner_name:?}: {e}"
                        )
                    })?;
                    write_0600(&token_path(root, spawner_name), &json).map_err(|e| {
                        anyhow::anyhow!(
                            "budget write-back failed for spawner {spawner_name:?}: {e} \
                             — mint refused (fail-closed: authority not granted without \
                             durable charge)"
                        )
                    })?;
                    Some(child_alloc)
                }
            };

            (child_budget, child_pub_req, child_sub_req)
        }
    } else {
        // Owner (no spawner session): no bus-scope check needed. The child
        // gets its requested/default scope unconditionally — zero behavior
        // change for the owner-spawned common case.
        let pub_vec = requested_publish.unwrap_or_else(|| vec![own_obs.clone()]);
        let sub_vec = requested_subscribe.unwrap_or_default();
        (requested_budget, pub_vec, sub_vec)
    };

    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let token = SessionToken {
        principal: principal.to_string(),
        agent: agent.to_string(),
        secret,
        owner_pid,
        grants: Grants {
            publish: child_publish,
            subscribe: child_subscribe,
            turn_budget: child_budget,
            remaining_budget: child_budget,
        },
    };
    write_0600(&token_path(root, principal), &serde_json::to_string(&token)?)?;
    Ok(token)
}

/// Read a session token by principal, rejecting path-unsafe names before any
/// file access (a crafted CONNECT username can never traverse the store). None if
/// not a session principal, absent, or unparseable.
pub fn read(root: &Root, principal: &str) -> Option<SessionToken> {
    if !is_session_principal(principal) {
        return None;
    }
    let raw = std::fs::read_to_string(token_path(root, principal)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Retire a session's token when the session ends — the credential dies with the
/// session, so a dead session's identity can never be re-presented.
pub fn retire(root: &Root, principal: &str) {
    if is_session_principal(principal) {
        let _ = std::fs::remove_file(token_path(root, principal));
    }
}

/// Reap orphaned session tokens: any token whose owning launcher pid is no
/// longer alive is a credential a SIGKILL leaked (the launcher never reached its
/// best-effort `retire`). Removing it makes the credential unusable — the broker
/// can no longer resolve it. Returns the principals reaped.
///
/// Run at daemon boot and launcher boot. Crash-only, same as every other
/// elanus liveness sweep (release_dead_leases, orphaned-dispatch cleanup).
pub fn reap_orphans(root: &Root) -> Vec<String> {
    let mut reaped = Vec::new();
    let dir = store_dir(root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return reaped;
    };
    for e in entries.filter_map(|e| e.ok()) {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(tok) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<SessionToken>(&s).ok())
        else {
            // Unparseable token file: not backing any resolvable session —
            // remove it rather than leave junk in the fenced store.
            let _ = std::fs::remove_file(&path);
            continue;
        };
        if !pid_alive(tok.owner_pid) {
            let _ = std::fs::remove_file(&path);
            reaped.push(tok.principal);
        }
    }
    reaped
}

/// Signal 0 is an existence probe with no effect — exactly how the daemon's
/// lease reaper (dispatcher::release_dead_leases) tests a holder pid. A `0`
/// return means the process exists and is signalable. A `-1` return must
/// distinguish `ESRCH` (no such process → dead) from `EPERM` (the process
/// exists but is owned by another uid → ALIVE): treat anything other than
/// `ESRCH` as alive, so a live cross-uid launcher's session token is never
/// wrongly reaped (fail-safe toward keeping a live session, not toward
/// dropping authority).
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn write_0600(path: &Path, contents: &str) -> std::io::Result<()> {
    // ATOMIC WRITE: write to a sibling temp file, set 0600, then rename into
    // place. On POSIX, rename(2) within a single filesystem is atomic — a
    // concurrent reader sees either the old complete file or the new complete
    // file, never a truncated/empty intermediate. This eliminates the torn-read
    // window that previously existed when truncate(true) zeroed the file before
    // write_all completed.
    //
    // Temp name: `<path>.tmp.<pid>.<counter>` — unique across processes and
    // across concurrent calls within the same process (the counter is per-
    // process and monotonically increasing).
    use std::sync::atomic::{AtomicU64, Ordering};
    static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "write_0600: path has no parent")
    })?;
    let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        "{}.tmp.{}.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("token"),
        std::process::id(),
        seq,
    );
    let tmp_path = parent.join(&tmp_name);

    // Write to the temp file, then atomically rename into place. On error at
    // any step, attempt to remove the temp file so we do not litter.
    let result = (|| -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&tmp_path)?.write_all(contents.as_bytes())?;
        std::fs::rename(&tmp_path, path)
    })();

    if result.is_err() {
        // Best-effort cleanup; ignore secondary errors.
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_root() -> Root {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "elanus-codesess-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    #[test]
    fn prefix_classification() {
        assert!(is_session_principal("code-deadbeef"));
        assert!(!is_session_principal("owner"));
        assert!(!is_session_principal("kernel"));
        assert!(!is_session_principal("recent-history"));
        // path-unsafe names are never session principals
        assert!(!is_session_principal("code-../owner"));
        assert!(!is_session_principal("code-a/b"));
    }

    #[test]
    fn mint_scope_is_only_the_own_obs_subtree() {
        let root = tmp_root();
        let tok = mint(&root, "code-deadbeef", "claude-code", 999_999, None, None, None, None).unwrap();
        // Publishes its own obs subtree …
        assert!(tok.may_publish("obs/agent/claude-code/code-deadbeef/session/start"));
        assert!(tok.may_publish("obs/agent/claude-code/code-deadbeef/tool/Bash/call"));
        // … and NOTHING else: not the owner mailbox, not work, not another
        // agent's obs, not another session's obs.
        assert!(!tok.may_publish("in/human/owner"));
        assert!(!tok.may_publish("work/agent/exec"));
        assert!(!tok.may_publish("in/agent/kestrel/c1"));
        assert!(!tok.may_publish("obs/agent/claude-code/code-other/session/start"));
        assert!(!tok.may_publish("obs/agent/codex/code-deadbeef/x"));
        // It may subscribe to nothing at all.
        assert!(!tok.may_subscribe("obs/#"));
        assert!(!tok.may_subscribe("obs/agent/claude-code/code-deadbeef/#"));
        assert!(!tok.may_subscribe("in/human/owner"));
    }

    #[test]
    fn roundtrip_and_retire() {
        let root = tmp_root();
        let minted = mint(&root, "code-cafef00d", "claude-code", 1234, None, None, None, None).unwrap();
        let read_back = read(&root, "code-cafef00d").unwrap();
        assert_eq!(read_back.secret, minted.secret);
        assert_eq!(read_back.agent, "claude-code");
        assert_eq!(read_back.owner_pid, 1234);
        retire(&root, "code-cafef00d");
        assert!(read(&root, "code-cafef00d").is_none());
    }

    #[test]
    fn read_rejects_non_session_and_unsafe_names() {
        let root = tmp_root();
        // a full-authority name is never resolved as a session token
        assert!(read(&root, "owner").is_none());
        assert!(read(&root, "code-../../owner").is_none());
    }

    #[test]
    fn pid_alive_treats_eperm_as_alive() {
        // pid 1 (init/launchd) exists but is owned by root; from a non-root
        // process `kill(1, 0)` returns EPERM, not ESRCH — it must read as ALIVE
        // so a live session owned by a different uid is never wrongly reaped.
        // (If the suite runs as root, kill(1,0) returns 0 — still alive.)
        assert!(pid_alive(1));
        // A pid that almost certainly does not exist reads as dead (ESRCH), and
        // non-positive pids are dead by definition.
        assert!(!pid_alive(0x7fff_fffe));
        assert!(!pid_alive(0));
        assert!(!pid_alive(-5));
    }

    #[test]
    fn reap_removes_dead_owner_keeps_live() {
        let root = tmp_root();
        // a token owned by a definitely-dead pid (pid 1 exists but we use a high
        // unlikely-live pid for the dead case; current pid for the live case)
        let dead_pid = 0x7fff_fffe; // not a live pid on any sane system
        mint(&root, "code-deadbeef", "claude-code", dead_pid, None, None, None, None).unwrap();
        let live = mint(
            &root,
            "code-livesess",
            "claude-code",
            std::process::id() as i32,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let reaped = reap_orphans(&root);
        assert!(reaped.contains(&"code-deadbeef".to_string()));
        // the orphan is gone, the live session's token survives
        assert!(read(&root, "code-deadbeef").is_none());
        assert!(read(&root, "code-livesess").is_some());
        let _ = live;
    }

    // ── The durable session RECORD (M2-A) ────────────────────────────────────

    #[test]
    fn record_roundtrips_and_carries_no_secret() {
        let root = tmp_root();
        // No record before a launch observes the native id.
        assert!(read_record(&root, "code-abcd1234").unwrap().is_none());

        let rec = SessionRecord {
            elanus_session: "code-abcd1234".to_string(),
            native_session: "019ee252-3d31-7681-b1d7-7a4b3c494fb5".to_string(),
            tool: "codex".to_string(),
            agent_noun: "codex".to_string(),
            workdir: "/tmp/proj".to_string(),
            room: None,
        };
        upsert_record(&root, &rec).unwrap();

        let read_back = read_record(&root, "code-abcd1234").unwrap().unwrap();
        assert_eq!(read_back, rec);
        // The record is the DURABLE half: it carries the native resume key and the
        // workdir, but NO secret (the token is the ephemeral half, minted per run).
        // Resume mints a fresh token from this record; the record itself never holds
        // a credential — proven by the struct having no secret field and the table
        // having no secret column (this row reads back identical without one).
    }

    #[test]
    fn record_upsert_refreshes_native_and_workdir_keyed_by_elanus_session() {
        let root = tmp_root();
        let mut rec = SessionRecord {
            elanus_session: "code-cafef00d".to_string(),
            native_session: "thread-1".to_string(),
            tool: "claude".to_string(),
            agent_noun: "claude-code".to_string(),
            workdir: "/tmp/a".to_string(),
            room: None,
        };
        upsert_record(&root, &rec).unwrap();
        // A re-observed native id / workdir (e.g. a second SessionStart) updates in
        // place rather than duplicating — the elanus session is the stable key.
        rec.native_session = "thread-2".to_string();
        rec.workdir = "/tmp/b".to_string();
        upsert_record(&root, &rec).unwrap();
        let read_back = read_record(&root, "code-cafef00d").unwrap().unwrap();
        assert_eq!(read_back.native_session, "thread-2");
        assert_eq!(read_back.workdir, "/tmp/b");

        // touch_record bumps last_active without disturbing the mapping.
        touch_record(&root, "code-cafef00d").unwrap();
        let again = read_record(&root, "code-cafef00d").unwrap().unwrap();
        assert_eq!(again.native_session, "thread-2");
    }

    #[test]
    fn delivery_key_claim_is_once_and_durable() {
        let root = tmp_root();
        // First claim of a key wins; the key is now seen (for that session).
        assert!(!delivery_key_seen(&root, "event:5", "code-x"));
        assert!(claim_delivery_key(&root, "event:5", "code-x", 5).unwrap());
        assert!(delivery_key_seen(&root, "event:5", "code-x"));
        // A second claim of the SAME key+session loses (a duplicate — the
        // at-least-once replay): it must NOT drive a second resume.
        assert!(!claim_delivery_key(&root, "event:5", "code-x", 5).unwrap());
        // A different key is independent.
        assert!(claim_delivery_key(&root, "planner-step-2", "code-y", 9).unwrap());
        // Durable across a fresh connection (a restart): the row is in the ledger,
        // so the replayed delivery is still recognized.
        assert!(delivery_key_seen(&root, "event:5", "code-x"));
        assert!(delivery_key_seen(&root, "planner-step-2", "code-y"));
        assert!(!delivery_key_seen(&root, "event:999", "code-x"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn delivery_key_is_namespaced_by_session_no_cross_victim_suppression() {
        // The cross-victim suppression probe (docs/security.md): an attacker
        // pre-claims an explicit key K for session A; a victim delivery to a
        // DIFFERENT session B reusing K must still be drivable (claim succeeds),
        // NOT falsely deduped. With a global key space the victim claim would lose;
        // namespacing by session keeps them independent.
        let root = tmp_root();
        let attacker_key = "shared-key-K";
        // Attacker pre-claims K for session A.
        assert!(claim_delivery_key(&root, attacker_key, "code-attackerA", 1).unwrap());
        assert!(delivery_key_seen(&root, attacker_key, "code-attackerA"));
        // The same key for the VICTIM's session B is NOT seen, so the victim
        // delivery drives (claim wins) — no suppression.
        assert!(!delivery_key_seen(&root, attacker_key, "code-victimB"));
        assert!(
            claim_delivery_key(&root, attacker_key, "code-victimB", 2).unwrap(),
            "victim delivery to a different session reusing the key must still drive"
        );
        // And the genuine replay (same key + SAME session) is still a no-op.
        assert!(!claim_delivery_key(&root, attacker_key, "code-victimB", 2).unwrap());
        assert!(delivery_key_seen(&root, attacker_key, "code-victimB"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn resume_mints_fresh_token_then_retires_no_idle_credential() {
        // The resume token lifecycle in isolation: an idle session has a record but
        // NO live token; a resume mints a fresh scoped token (emit-only) and retires
        // it — leaving no idle credential, exactly as a launch does. (The full
        // resume() also runs the tool; here we assert the credential property the
        // resume primitive must preserve.)
        let root = tmp_root();
        let rec = SessionRecord {
            elanus_session: "code-resume01".to_string(),
            native_session: "thread-x".to_string(),
            tool: "codex".to_string(),
            agent_noun: "codex".to_string(),
            workdir: "/tmp/proj".to_string(),
            room: None,
        };
        upsert_record(&root, &rec).unwrap();
        // Idle: record present, no token.
        assert!(read_record(&root, "code-resume01").unwrap().is_some());
        assert!(read(&root, "code-resume01").is_none());

        // Resume mints a fresh, emit-only token …
        let token = mint(&root, "code-resume01", "codex", std::process::id() as i32, None, None, None, None).unwrap();
        assert!(token.may_publish("obs/agent/codex/code-resume01/session/resume"));
        assert!(!token.may_publish("in/human/owner"));
        assert!(token.grants.subscribe.is_empty(), "resume token must be emit-only");
        assert!(read(&root, "code-resume01").is_some());

        // … and retires it: no idle credential survives the resume.
        retire(&root, "code-resume01");
        assert!(read(&root, "code-resume01").is_none());
        // The durable record outlives the token — still resumable later.
        assert!(read_record(&root, "code-resume01").unwrap().is_some());
    }

    // ── The session inbox + memory note (M3) ─────────────────────────────────

    /// Emit a delivery into a session's mailbox via the kernel ledger, exactly as
    /// a real deliver/owner publish does, so the inbox read sees a genuine row.
    fn deliver_into(root: &Root, noun: &str, session: &str, sender: &str, msg: &str) -> i64 {
        let conn = crate::db::open(root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let topic = format!(
            "in/agent/{}/{}",
            crate::topic::encode_segment(noun),
            crate::topic::encode_segment(session),
        );
        crate::events::emit(
            root,
            &conn,
            crate::events::EmitOpts {
                payload: Some(serde_json::json!({ "prompt": msg })),
                sender: Some(sender.to_string()),
                ..crate::events::EmitOpts::new(&topic)
            },
        )
        .unwrap()
    }

    #[test]
    fn inbox_reads_only_the_sessions_own_mailbox() {
        // THE CRUX (M3 read-scoping): a session's inbox read returns ITS OWN
        // deliveries and NEVER another session's, because the mailbox topic is
        // built from the (env-derived) own identity — there is no code path that
        // names a different session's mailbox.
        let root = tmp_root();
        upsert_record(
            &root,
            &SessionRecord {
                elanus_session: "code-mine0001".into(),
                native_session: "t1".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: "/tmp".into(),
                room: None,
            },
        )
        .unwrap();
        upsert_record(
            &root,
            &SessionRecord {
                elanus_session: "code-other002".into(),
                native_session: "t2".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: "/tmp".into(),
                room: None,
            },
        )
        .unwrap();
        // Two deliveries to MINE, one to OTHER.
        deliver_into(&root, "codex", "code-mine0001", "owner", "for me #1");
        deliver_into(&root, "codex", "code-mine0001", "code-planner", "for me #2");
        deliver_into(&root, "codex", "code-other002", "owner", "for someone else");

        // My inbox, read by MY identity, has exactly my two — never the other's.
        let mine = inbox_for_session(&root, "codex", "code-mine0001", false).unwrap();
        assert_eq!(mine.len(), 2);
        let msgs: Vec<&str> = mine.iter().map(|i| i.message.as_str()).collect();
        assert!(msgs.contains(&"for me #1"));
        assert!(msgs.contains(&"for me #2"));
        assert!(!msgs.iter().any(|m| m.contains("someone else")));
        // who-from + correlation are surfaced.
        assert_eq!(mine[1].from.as_deref(), Some("code-planner"));

        // The other session reads its own one delivery (proof the scoping cuts the
        // other way too — neither can see the other's).
        let other = inbox_for_session(&root, "codex", "code-other002", false).unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].message, "for someone else");

        // The mailbox topic is derived from identity, so even passing a DELIBERATELY
        // mismatched noun for my session simply reads a DIFFERENT (empty) topic — it
        // can never read another real session's inbox. (There is no parameter that
        // lets `code-mine0001`'s caller read `code-other002`'s rows.)
        let wrong_noun = inbox_for_session(&root, "claude-code", "code-mine0001", false).unwrap();
        assert!(wrong_noun.is_empty(), "a different noun reads its own empty mailbox, not another session's");

        // A non-session name has no inbox at all (no crafted topic).
        assert!(inbox_for_session(&root, "codex", "owner", false).unwrap().is_empty());
        assert!(inbox_for_session(&root, "codex", "code-../escape", false).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn inbox_seen_is_idempotent_and_scopes_unseen() {
        let root = tmp_root();
        upsert_record(
            &root,
            &SessionRecord {
                elanus_session: "code-seen0001".into(),
                native_session: "t".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: "/tmp".into(),
                room: None,
            },
        )
        .unwrap();
        let id1 = deliver_into(&root, "codex", "code-seen0001", "owner", "one");
        let _id2 = deliver_into(&root, "codex", "code-seen0001", "owner", "two");

        // Both start unseen.
        assert_eq!(inbox_for_session(&root, "codex", "code-seen0001", true).unwrap().len(), 2);
        // Mark the first seen — only one remains unseen, idempotently.
        mark_inbox_seen(&root, "code-seen0001", &[id1]).unwrap();
        mark_inbox_seen(&root, "code-seen0001", &[id1]).unwrap(); // re-mark = no-op
        let unseen = inbox_for_session(&root, "codex", "code-seen0001", true).unwrap();
        assert_eq!(unseen.len(), 1);
        assert_eq!(unseen[0].message, "two");
        // The full inbox still shows both, with the seen flag honest.
        let all = inbox_for_session(&root, "codex", "code-seen0001", false).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().find(|i| i.event_id == id1).unwrap().seen);
        // A session can only mark ITS OWN deliveries seen: marking under a
        // different session id touches a different (session, event) keyspace and
        // does NOT hide my unseen row.
        mark_inbox_seen(&root, "code-attacker9", &[unseen[0].event_id]).unwrap();
        assert_eq!(inbox_for_session(&root, "codex", "code-seen0001", true).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn note_round_trips_and_clears() {
        let root = tmp_root();
        assert!(get_note(&root, "code-note0001").unwrap().is_none());
        set_note(&root, "code-note0001", "  remember the migration  ").unwrap();
        assert_eq!(get_note(&root, "code-note0001").unwrap().as_deref(), Some("remember the migration"));
        // Replacing the note shows the new text.
        set_note(&root, "code-note0001", "actually do the rename first").unwrap();
        assert_eq!(
            get_note(&root, "code-note0001").unwrap().as_deref(),
            Some("actually do the rename first")
        );
        // An empty note clears it.
        set_note(&root, "code-note0001", "   ").unwrap();
        assert!(get_note(&root, "code-note0001").unwrap().is_none());
        // A note can't attach to a non-session name.
        assert!(set_note(&root, "owner", "x").is_err());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── M5: coordination room membership + advisory edit claims ───────────────

    fn member(root: &Root, room: &str, session: &str, pid: i32) {
        // Each member also needs a record carrying its room (set_room), so a
        // claim's room can be derived from identity in the higher layers; here we
        // exercise the room-keyed primitives directly.
        upsert_record(
            root,
            &SessionRecord {
                elanus_session: session.into(),
                native_session: "t".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: "/tmp".into(),
                room: Some(room.into()),
            },
        )
        .unwrap();
        join_room(root, room, session, "codex", pid).unwrap();
    }

    #[test]
    fn claim_round_trips_and_peer_view_excludes_own() {
        // THE M5 CRUX: a session sees its ROOMMATES' claims, never its own, in its
        // room — and a claim recording never blocks anyone (it's advisory).
        let root = tmp_root();
        let live = std::process::id() as i32;
        member(&root, "room-1", "code-aaaa0001", live);
        member(&root, "room-1", "code-bbbb0002", live);

        // A claims a file; B claims another.
        add_claim(&root, "room-1", "code-aaaa0001", "src/foo.rs").unwrap();
        add_claim(&root, "room-1", "code-bbbb0002", "src/bar.rs").unwrap();

        // B's peer view shows A's claim and EXCLUDES B's own.
        let b_peers = peer_claims(&root, "room-1", "code-bbbb0002").unwrap();
        assert_eq!(b_peers.len(), 1);
        assert_eq!(b_peers[0].session, "code-aaaa0001");
        assert_eq!(b_peers[0].path, "src/foo.rs");
        // A's peer view shows B's, excludes A's own.
        let a_peers = peer_claims(&root, "room-1", "code-aaaa0001").unwrap();
        assert_eq!(a_peers.len(), 1);
        assert_eq!(a_peers[0].session, "code-bbbb0002");
        // A's own-claims view shows only A's.
        let a_own = own_claims(&root, "room-1", "code-aaaa0001").unwrap();
        assert_eq!(a_own.len(), 1);
        assert_eq!(a_own[0].path, "src/foo.rs");

        // Re-claiming the same path is idempotent (no duplicate row).
        add_claim(&root, "room-1", "code-aaaa0001", "src/foo.rs").unwrap();
        assert_eq!(own_claims(&root, "room-1", "code-aaaa0001").unwrap().len(), 1);

        // unclaim clears only the holder's own claim; idempotent.
        assert!(remove_claim(&root, "room-1", "code-aaaa0001", "src/foo.rs").unwrap());
        assert!(!remove_claim(&root, "room-1", "code-aaaa0001", "src/foo.rs").unwrap());
        assert!(peer_claims(&root, "room-1", "code-bbbb0002").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn rooms_are_isolated_no_cross_room_claim_leak() {
        // A session in room R1 must NOT see claims from room R2.
        let root = tmp_root();
        let live = std::process::id() as i32;
        member(&root, "R1", "code-r1a00001", live);
        member(&root, "R2", "code-r2a00001", live);
        add_claim(&root, "R1", "code-r1a00001", "r1/file.rs").unwrap();
        add_claim(&root, "R2", "code-r2a00001", "r2/file.rs").unwrap();
        // A roommate-less viewer in R1 sees R1's claim, never R2's.
        member(&root, "R1", "code-r1b00002", live);
        let r1_peers = peer_claims(&root, "R1", "code-r1b00002").unwrap();
        assert_eq!(r1_peers.len(), 1);
        assert_eq!(r1_peers[0].path, "r1/file.rs");
        // R2's claims are entirely invisible to an R1 query.
        assert!(!r1_peers.iter().any(|c| c.path.contains("r2/")));
        // An empty room id returns nothing (a solo session has no peers).
        assert!(peer_claims(&root, "", "code-r1b00002").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn own_write_only_a_session_cannot_forge_a_claim_as_another() {
        // The write primitives are keyed by the (room, session) the CALLER supplies,
        // but every caller in the CLI derives that from its OWN env-derived identity
        // (session_room_identity) — there is no path that lets a session pass a peer's
        // id. Here we assert the primitive faithfully attributes a claim to the
        // session it is told, and that clearing is scoped to that session: removing
        // "as A" never touches B's claim on the same path.
        let root = tmp_root();
        let live = std::process::id() as i32;
        member(&root, "room-x", "code-realA001", live);
        member(&root, "room-x", "code-realB002", live);
        add_claim(&root, "room-x", "code-realA001", "shared.rs").unwrap();
        add_claim(&root, "room-x", "code-realB002", "shared.rs").unwrap();
        // A "unclaim" scoped to A removes ONLY A's row; B's claim on the same path
        // survives (a session can't clear a peer's claim).
        assert!(remove_claim(&root, "room-x", "code-realA001", "shared.rs").unwrap());
        let b_still = own_claims(&root, "room-x", "code-realB002").unwrap();
        assert_eq!(b_still.len(), 1);
        assert_eq!(b_still[0].session, "code-realB002");
        // A non-session name can never hold a claim (add_claim rejects it).
        assert!(add_claim(&root, "room-x", "owner", "x").is_err());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn unclaim_releases_a_path_from_peers_view() {
        // A session releasing a path with `unclaim` stops its peers from seeing that
        // claim — the explicit-release production path (the other release path is the
        // crash reaper, tested below). A session that finishes editing a file frees
        // its peers to touch it.
        let root = tmp_root();
        let live = std::process::id() as i32;
        member(&root, "room-r", "code-doneone1", live);
        member(&root, "room-r", "code-stays002", live);
        add_claim(&root, "room-r", "code-doneone1", "a.rs").unwrap();
        add_claim(&root, "room-r", "code-doneone1", "b.rs").unwrap();
        add_claim(&root, "room-r", "code-stays002", "c.rs").unwrap();
        // Before: the stayer sees the worker's two claims.
        assert_eq!(peer_claims(&root, "room-r", "code-stays002").unwrap().len(), 2);
        // The worker finishes a.rs and releases it; b.rs is still held.
        assert!(remove_claim(&root, "room-r", "code-doneone1", "a.rs").unwrap());
        let peers = peer_claims(&root, "room-r", "code-stays002").unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].path, "b.rs");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn reap_dead_members_releases_a_sigkilled_sessions_claims_keeps_live() {
        // A SIGKILL'd session (dead owner pid) has its membership + claims reaped, so
        // they don't linger in roommates' injections forever. A live session's claims
        // survive the sweep.
        let root = tmp_root();
        let dead_pid = 0x7fff_fffe; // not a live pid on any sane system
        let live_pid = std::process::id() as i32;
        member(&root, "room-z", "code-deadone1", dead_pid);
        member(&root, "room-z", "code-liveone2", live_pid);
        add_claim(&root, "room-z", "code-deadone1", "dead.rs").unwrap();
        add_claim(&root, "room-z", "code-liveone2", "live.rs").unwrap();
        let reaped = reap_dead_members(&root);
        assert!(reaped.contains(&("room-z".to_string(), "code-deadone1".to_string())));
        // The dead session's claim is gone; the live session's survives.
        let live_view = own_claims(&root, "room-z", "code-liveone2").unwrap();
        assert_eq!(live_view.len(), 1);
        // The live session no longer sees the dead peer.
        assert!(peer_claims(&root, "room-z", "code-liveone2").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn upsert_preserves_a_room_set_at_launch() {
        // set_room writes the room; a later native-id upsert carrying room:None
        // (the CC SessionStart / codex thread.started path) must PRESERVE it
        // (COALESCE), not clear it — otherwise a session would lose its room after
        // the first observation.
        let root = tmp_root();
        set_room(&root, "code-keep0001", "my-room").unwrap();
        // The native-id upsert arrives with room:None.
        upsert_record(
            &root,
            &SessionRecord {
                elanus_session: "code-keep0001".into(),
                native_session: "thread-9".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: "/proj".into(),
                room: None,
            },
        )
        .unwrap();
        let rec = read_record(&root, "code-keep0001").unwrap().unwrap();
        assert_eq!(rec.room.as_deref(), Some("my-room"));
        assert_eq!(rec.native_session, "thread-9"); // the rest filled in
        assert_eq!(rec.workdir, "/proj");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── M1: budget dimension + Σ≤parent assert ───────────────────────────────
    //
    // The regression tests for docs/handoffs/authority-delegation.md M1 and
    // docs/security.md entry 22. They assert at the MINT LAYER — the level
    // entry 20's fix taught us to test: authority, not shape.

    #[test]
    fn budget_unbounded_owner_path_is_zero_behavior_change() {
        // THE BASELINE: the owner spawns a session directly (no spawner session
        // token in the fenced store). spawner=None, requested_budget=None →
        // unbounded — turn_budget and remaining_budget are both None.
        // This is the existing path; the test proves it is unchanged.
        let root = tmp_root();
        let tok = mint(&root, "code-budget001", "claude-code", 999_999, None, None, None, None).unwrap();
        assert_eq!(tok.grants.turn_budget, None, "owner path must be unbounded");
        assert_eq!(tok.grants.remaining_budget, None, "owner path must be unbounded");
        // Roundtrip: the token file deserializes back with None budgets.
        let read_back = read(&root, "code-budget001").unwrap();
        assert_eq!(read_back.grants.turn_budget, None);
        assert_eq!(read_back.grants.remaining_budget, None);
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn budget_inherit_equal_child_gets_parents_full_remaining() {
        // When a spawner has a finite budget and the child makes no explicit
        // request (requested_budget=None), the child inherits the spawner's
        // full remaining — the inherit-equal default (handoff open decision 1).
        let root = tmp_root();
        let parent_pid = std::process::id() as i32;
        // Mint the parent with a finite budget (e.g. 100 turns).
        mint(&root, "code-parent01", "claude-code", parent_pid, None, Some(100), None, None).unwrap();
        // Child inherits full remaining (100) via inherit-equal.
        let child = mint(&root, "code-child001", "claude-code", parent_pid,
                         Some("code-parent01"), None, None, None).unwrap();
        assert_eq!(child.grants.turn_budget, Some(100), "inherit-equal: child gets parent's full remaining");
        assert_eq!(child.grants.remaining_budget, Some(100));
        // Parent's remaining is now 0 (the child took the full 100).
        let parent_after = read(&root, "code-parent01").unwrap();
        assert_eq!(parent_after.grants.remaining_budget, Some(0),
                   "parent's remaining is decremented by the child's allocation");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn budget_explicit_narrow_child_gets_requested_amount() {
        // When the child explicitly requests a smaller budget than the parent's
        // remaining, it gets exactly what it asked for — narrowing is the point
        // of the RLM case ("halve it to pass context down").
        let root = tmp_root();
        let pid = std::process::id() as i32;
        mint(&root, "code-parent02", "claude-code", pid, None, Some(100), None, None).unwrap();
        let child = mint(&root, "code-child002", "claude-code", pid,
                         Some("code-parent02"), Some(40), None, None).unwrap();
        assert_eq!(child.grants.turn_budget, Some(40), "explicit narrowing: child gets requested 40");
        assert_eq!(child.grants.remaining_budget, Some(40));
        // Parent's remaining decremented by 40.
        let parent_after = read(&root, "code-parent02").unwrap();
        assert_eq!(parent_after.grants.remaining_budget, Some(60));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn budget_child_cannot_exceed_parent_remaining() {
        // THE CRUX (security.md entry 22): a child requesting more turns than the
        // spawner has remaining MUST be refused at mint — the spawn does not happen.
        // This is the monotone-narrowing invariant: no spawn may widen authority.
        let root = tmp_root();
        let pid = std::process::id() as i32;
        // Parent has 50 turns remaining.
        mint(&root, "code-parent03", "claude-code", pid, None, Some(50), None, None).unwrap();
        // Child requests 51 — one more than the parent has.
        let result = mint(&root, "code-child003", "claude-code", pid,
                          Some("code-parent03"), Some(51), None, None);
        assert!(result.is_err(), "over-allocation must be refused at mint");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("budget allocation refused"),
                "error must name the refusal class: {err}");
        assert!(err.contains("code-parent03"),
                "error must name the spawner: {err}");
        // The refused mint must NOT have written a child token.
        assert!(read(&root, "code-child003").is_none(),
                "refused mint must not leave a token file");
        // The parent's remaining is unchanged — the failed mint must not charge it.
        let parent_after = read(&root, "code-parent03").unwrap();
        assert_eq!(parent_after.grants.remaining_budget, Some(50),
                   "failed mint must not decrement the spawner's remaining");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn budget_sigma_siblings_partition_parent() {
        // The Σ (sigma) invariant: siblings PARTITION the parent's budget, not
        // share it. The second sibling that would push the cumulative allocation
        // over the parent's remaining is refused — this is the "Σ children ≤
        // parent" from the handoff, not just "each child ≤ parent".
        let root = tmp_root();
        let pid = std::process::id() as i32;
        // Parent starts with 60 turns.
        mint(&root, "code-parent04", "claude-code", pid, None, Some(60), None, None).unwrap();

        // First child claims 40: succeeds; parent now has 20 remaining.
        let c1 = mint(&root, "code-sib001", "claude-code", pid,
                      Some("code-parent04"), Some(40), None, None).unwrap();
        assert_eq!(c1.grants.turn_budget, Some(40));
        let after_c1 = read(&root, "code-parent04").unwrap();
        assert_eq!(after_c1.grants.remaining_budget, Some(20));

        // Second child claims 21: would push Σ to 61 > 60 → REFUSED.
        let result = mint(&root, "code-sib002", "claude-code", pid,
                          Some("code-parent04"), Some(21), None, None);
        assert!(result.is_err(), "second sibling must be refused when Σ > parent");
        assert!(read(&root, "code-sib002").is_none(),
                "refused sibling must not leave a token");

        // Parent's remaining is still 20 (the failed mint did not charge it).
        let after_fail = read(&root, "code-parent04").unwrap();
        assert_eq!(after_fail.grants.remaining_budget, Some(20));

        // Third sibling that fits (20) succeeds — the partition has exactly 0 left.
        let c3 = mint(&root, "code-sib003", "claude-code", pid,
                      Some("code-parent04"), Some(20), None, None).unwrap();
        assert_eq!(c3.grants.turn_budget, Some(20));
        let after_c3 = read(&root, "code-parent04").unwrap();
        assert_eq!(after_c3.grants.remaining_budget, Some(0),
                   "siblings exactly exhaust the parent's budget — Σ = parent");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn budget_old_token_without_field_deserializes_as_unbounded() {
        // Backward-compatibility: a token file written BEFORE M1 (no turn_budget /
        // remaining_budget fields) must deserialize with both fields as None so
        // existing sessions are treated as unbounded — zero behavior change for
        // tokens that predate the M1 serialization.
        let root = tmp_root();
        let dir = store_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        // Write a token in the pre-M1 shape (no budget fields).
        let legacy_json = r#"{
            "principal":"code-legacy01",
            "agent":"claude-code",
            "secret":"abc123",
            "owner_pid":1,
            "publish":["obs/agent/claude-code/code-legacy01/#"],
            "subscribe":[]
        }"#;
        let path = dir.join("code-legacy01.json");
        std::fs::write(&path, legacy_json).unwrap();
        let tok = read(&root, "code-legacy01").expect("legacy token must be readable");
        assert_eq!(tok.grants.turn_budget, None,
                   "missing turn_budget in old token → None (unbounded)");
        assert_eq!(tok.grants.remaining_budget, None,
                   "missing remaining_budget in old token → None (unbounded)");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── M2: bus-scope child ⊆ spawner (docs/handoffs/authority-delegation.md) ───

    /// Helper: mint a spawner, then overwrite its grants to a specific bus scope.
    fn spawner_with_scope(root: &Root, name: &str, publish: &[&str], subscribe: &[&str]) {
        let mut sp = mint(root, name, "claude-code", 999_999, None, None, None, None).unwrap();
        sp.grants.publish = publish.iter().map(|s| s.to_string()).collect();
        sp.grants.subscribe = subscribe.iter().map(|s| s.to_string()).collect();
        write_0600(&token_path(root, name), &serde_json::to_string(&sp).unwrap()).unwrap();
    }

    #[test]
    fn m2_owner_spawned_default_scope_is_unchanged() {
        // The common case (no spawner) is byte-identical to entry-20/M1: publish
        // exactly the own obs subtree, subscribe nothing.
        let root = tmp_root();
        let tok = mint(&root, "code-ownerdef", "claude-code", 1, None, None, None, None).unwrap();
        assert_eq!(tok.grants.publish, vec!["obs/agent/claude-code/code-ownerdef/#".to_string()]);
        assert!(tok.grants.subscribe.is_empty());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m2_child_publish_must_be_subset_of_spawner() {
        let root = tmp_root();
        spawner_with_scope(&root, "code-scopep1", &["obs/agent/claude-code/#"], &[]);
        // A child requesting a WIDER publish (obs/#) than the spawner is refused.
        let widen = mint(&root, "code-cwide1", "claude-code", 1, Some("code-scopep1"),
                         None, Some(vec!["obs/#".to_string()]), None);
        assert!(widen.is_err(), "widening publish must be refused");
        let err = widen.unwrap_err().to_string();
        assert!(err.contains("bus publish refused"), "must name the refusal: {err}");
        // A child requesting another agent's subtree (not its own, not under
        // spawner's claude-code-only grant) is refused.
        let cross = mint(&root, "code-ccross1", "claude-code", 1, Some("code-scopep1"),
                         None, Some(vec!["obs/agent/codex/code-ccross1/#".to_string()]), None);
        assert!(cross.is_err(), "cross-agent publish must be refused");
        // A child requesting a filter under the spawner's grant succeeds.
        let ok = mint(&root, "code-cok1", "claude-code", 1, Some("code-scopep1"),
                      None, Some(vec!["obs/agent/claude-code/code-cok1/#".to_string()]), None);
        assert!(ok.is_ok(), "publish ⊆ spawner must pass: {ok:?}");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m2_child_own_obs_always_allowed_even_if_spawner_narrower() {
        // The child's own self-telemetry subtree is its own audit trail and is
        // ALWAYS allowed, even when the spawner's grant does not cover it.
        let root = tmp_root();
        // Spawner can only publish a DISJOINT subtree (a different agent).
        spawner_with_scope(&root, "code-scopep2", &["obs/agent/codex/#"], &[]);
        let own = "obs/agent/claude-code/code-cown2/#".to_string();
        let tok = mint(&root, "code-cown2", "claude-code", 1, Some("code-scopep2"),
                       None, Some(vec![own.clone()]), None).unwrap();
        assert_eq!(tok.grants.publish, vec![own]);
        // But the child CANNOT borrow the spawner's disjoint subtree for itself
        // unless it explicitly requests it AND it is covered — requesting codex's
        // subtree IS covered by the spawner here, so that is legitimately allowed
        // (it is ⊆ spawner). Requesting something neither own nor ⊆ spawner fails.
        let bad = mint(&root, "code-cown2b", "claude-code", 1, Some("code-scopep2"),
                       None, Some(vec!["in/human/owner".to_string()]), None);
        assert!(bad.is_err(), "a filter neither own nor ⊆ spawner must be refused");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m2_default_child_publish_passes_against_narrow_spawner() {
        // With no requested_publish, the child's default is its own obs subtree —
        // which is always allowed — so a narrow spawner never blocks the default.
        let root = tmp_root();
        spawner_with_scope(&root, "code-scopep3", &["obs/agent/codex/#"], &[]);
        let tok = mint(&root, "code-cdef3", "claude-code", 1, Some("code-scopep3"),
                       None, None, None).unwrap();
        assert_eq!(tok.grants.publish, vec!["obs/agent/claude-code/code-cdef3/#".to_string()]);
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m2_child_subscribe_must_be_subset_of_spawner() {
        let root = tmp_root();
        // Spawner has a non-empty subscribe scope.
        spawner_with_scope(&root, "code-scopes1", &["obs/agent/claude-code/#"],
                           &["obs/agent/claude-code/#"]);
        // A child subscribe not covered by the spawner's is refused.
        let widen = mint(&root, "code-csub1", "claude-code", 1, Some("code-scopes1"),
                         None, None, Some(vec!["obs/#".to_string()]));
        assert!(widen.is_err(), "widening subscribe must be refused");
        let err = widen.unwrap_err().to_string();
        assert!(err.contains("bus subscribe refused"), "must name the refusal: {err}");
        // A covered subscribe passes.
        let ok = mint(&root, "code-csub2", "claude-code", 1, Some("code-scopes1"),
                      None, None, Some(vec!["obs/agent/claude-code/code-x/#".to_string()]));
        assert!(ok.is_ok(), "subscribe ⊆ spawner must pass: {ok:?}");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m2_default_empty_subscribe_passes_under_empty_spawner() {
        // The default child subscribe is empty, which trivially satisfies ⊆ even
        // when the spawner's subscribe is also empty (today's structural default).
        let root = tmp_root();
        spawner_with_scope(&root, "code-scopes2", &["obs/agent/claude-code/#"], &[]);
        let tok = mint(&root, "code-csub3", "claude-code", 1, Some("code-scopes2"),
                       None, None, None).unwrap();
        assert!(tok.grants.subscribe.is_empty());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m2_flat_m1_token_json_still_deserializes() {
        // serde(flatten) back-compat: an M1-era flat token (publish/subscribe/
        // turn_budget/remaining_budget at top level) deserializes into Grants.
        let m1 = r#"{"principal":"code-m1tok","agent":"codex","secret":"s","owner_pid":3,
                     "publish":["obs/agent/codex/code-m1tok/#"],"subscribe":[],
                     "turn_budget":5,"remaining_budget":2}"#;
        let tok: SessionToken = serde_json::from_str(m1).unwrap();
        assert_eq!(tok.grants.publish, vec!["obs/agent/codex/code-m1tok/#".to_string()]);
        assert_eq!(tok.grants.turn_budget, Some(5));
        assert_eq!(tok.grants.remaining_budget, Some(2));
        // And it re-serializes to the same flat shape (no nested "grants" key).
        let back = serde_json::to_string(&tok).unwrap();
        assert!(back.contains("\"publish\""), "publish stays top-level: {back}");
        assert!(!back.contains("\"grants\""), "no nested grants key: {back}");
    }

    #[test]
    fn budget_concurrent_siblings_cannot_exceed_parent_via_race() {
        // REGRESSION TEST FOR FINDING 1 (HIGH — concurrent-sibling TOCTOU).
        //
        // Two fixes close this race:
        //   1. write_0600 uses atomic rename(2) — readers never see a torn file.
        //   2. The pre-lock branch decision uses Path::try_exists (stable POSIX
        //      signal), not a parse result that could return None on a partial read.
        //
        // This test MUST FAIL reliably if either fix is reverted. It hammers the
        // race window across 60 iterations, each spawning 12 threads against a
        // budget that only 3 can succeed — 9× oversub, 60× repetition.
        //
        // Assertions per iteration:
        //   (a) Σ of granted child budgets ≤ parent_start.
        //   (b) Exactly floor(parent_budget / per_child) children succeed.
        //   (c) The parent's final persisted remaining == parent_start − Σgranted.
        //
        // Threads open separate fds, so flock correctly provides mutual exclusion
        // (same as separate processes — lock is per open-file-description, not per-pid).
        use std::sync::{Arc, Mutex};
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};

        let pid = std::process::id() as i32;

        // Parent: 30 turns total. Each child requests 10 → 3 succeed, 9 are refused.
        let parent_budget: u64 = 30;
        let per_child: u64 = 10;
        let expected_successes: usize = (parent_budget / per_child) as usize;
        // 12 threads competing for 3 slots — 4× oversub per iteration.
        let n_threads: usize = 12;
        // 60 iterations hammers the race window far more reliably than a single run.
        let n_iters: usize = 60;

        // Unique name counter so each iteration gets fresh principals (no leftover
        // token state from a previous iteration leaking through).
        static ITER_CTR: AtomicUsize = AtomicUsize::new(0);

        for _iter in 0..n_iters {
            let ctr = ITER_CTR.fetch_add(1, AOrdering::Relaxed);
            let root = Arc::new(tmp_root());
            let parent_name = format!("code-racep{ctr:04}");

            mint(&root, &parent_name, "claude-code", pid, None, Some(parent_budget), None, None).unwrap();

            let successes: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));

            let handles: Vec<_> = (0..n_threads)
                .map(|i| {
                    let root = Arc::clone(&root);
                    let successes = Arc::clone(&successes);
                    let parent_name = parent_name.clone();
                    std::thread::spawn(move || {
                        let child_name = format!("code-racec{ctr:04}t{i:02}");
                        let result = mint(
                            &root,
                            &child_name,
                            "claude-code",
                            pid,
                            Some(&parent_name),
                            Some(per_child),
                            None,
                            None,
                        );
                        if let Ok(tok) = result {
                            let granted = tok.grants.turn_budget.unwrap_or(0);
                            successes.lock().unwrap().push(granted);
                        }
                    })
                })
                .collect();

            for h in handles {
                h.join().expect("thread panicked");
            }

            let granted_vec = successes.lock().unwrap().clone();
            let n_succeeded = granted_vec.len();
            let sigma: u64 = granted_vec.iter().sum();

            // (a) Σ granted must never exceed the parent budget — the invariant.
            assert!(
                sigma <= parent_budget,
                "iter {_iter}: Σ granted ({sigma}) must not exceed parent budget \
                 ({parent_budget}) — concurrency bug"
            );

            // (b) Exactly the expected number of children succeed.
            assert_eq!(
                n_succeeded, expected_successes,
                "iter {_iter}: expected {expected_successes} successes \
                 (budget={parent_budget}, per_child={per_child}); got {n_succeeded}"
            );

            // (c) The parent's final persisted remaining equals parent_start − Σgranted.
            let parent_final = read(&root, &parent_name)
                .expect("parent token must still be readable after concurrent mints");
            let final_remaining = parent_final.grants.remaining_budget
                .expect("parent remaining must be Some after finite-budget mints");
            assert_eq!(
                final_remaining,
                parent_budget - sigma,
                "iter {_iter}: parent remaining ({final_remaining}) must equal \
                 parent_start ({parent_budget}) − Σgranted ({sigma})"
            );

            let _ = std::fs::remove_dir_all(&root.dir);
        }
    }

    #[test]
    fn budget_unparseable_spawner_token_fails_closed() {
        // REGRESSION TEST FOR FAIL-CLOSED CLASSIFICATION (belt-and-suspenders).
        //
        // If the spawner token file exists but is corrupt/unparseable, mint()
        // must REFUSE the spawn — not treat it as unbounded authority.
        // Previously (before the fix), a None parse result from a torn read
        // was indistinguishable from "no token" and granted the child.
        let root = tmp_root();
        let pid = std::process::id() as i32;
        let dir = store_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();

        // Write a syntactically invalid JSON file at the spawner token path.
        let path = dir.join("code-corruptparent.json");
        std::fs::write(&path, b"{ THIS IS NOT VALID JSON }").unwrap();

        // Attempting to mint a child with this as spawner must fail.
        let result = mint(
            &root,
            "code-child-of-corrupt",
            "claude-code",
            pid,
            Some("code-corruptparent"),
            Some(10),
            None,
            None,
        );
        assert!(
            result.is_err(),
            "mint with unparseable spawner token must be refused (fail-closed)"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("corrupt") || err.contains("parsed") || err.contains("could not be"),
            "error must explain the refusal reason: {err}"
        );
        // No child token must have been written.
        assert!(
            read(&root, "code-child-of-corrupt").is_none(),
            "refused mint must not write a child token"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }
}
