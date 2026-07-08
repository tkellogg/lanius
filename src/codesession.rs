//! Per-session coding-agent identity: a **grant-scoped** session actor token.
//!
//! A coding session (`lanius code launch claude …`) must publish to the bus as
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
// in `lanius.db` (`code_sessions`), carries **no secret**, and survives process
// exit: it maps the lanius session id to the tool's own native resumable session
// id (codex `thread_id` / CC `session_id`), the tool, the agent noun, and the
// workdir. An idle resumable session is exactly this — a record with no live
// token. `lanius code resume` reads the record to mint a FRESH scoped token and
// continue the native session in its recorded workdir, then retires the token.
// This preserves the verified "no idle live credential" property while enabling
// resume: the credential is per-run, the record is durable.

/// A durable coding-session record (the `code_sessions` row). Carries no secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// The lanius session id (`code-<8hex>`), the stable handle a human resumes.
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
/// lanius session: a re-observed native id (e.g. a second SessionStart) refreshes
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

// ── SA2: live siblings in the same workdir (the per-turn injection roster) ─────
//
// docs/handoffs/sibling-awareness.md SA2. The per-turn injection prepends one
// line naming the OTHER live sessions in this session's workdir. The roster +
// liveness come from `code_sessions` (which carries `workdir` + `last_active` —
// the column the upsert bumps but `SessionRecord`/`read_record` don't surface);
// rather than widen `SessionRecord` (and every struct literal across the tree),
// SA2 uses this DEDICATED sibling query.
//
// LIVENESS, DEFINED HONESTLY (the handoff guardrail): a sibling counts as LIVE
// only if it is BOTH plausibly current and not a crashed ghost:
//   • `last_active` within a freshness window (LIVE_WINDOW_SECS), AND
//   • its room-membership `owner_pid` is still alive (signal-0 probe), when a
//     membership row exists. A SIGKILL'd session whose pid is gone ages out
//     immediately; a session with no membership row falls back to the time
//     window alone. Either signal going stale drops it from the roster, so a
//     dead session never haunts a sibling's injection (stale-session hygiene).

/// How fresh a session's `last_active` must be to count as a live sibling. A
/// crashed session whose `last_active` is older than this ages out even if its
/// owner pid is somehow still reported (belt-and-suspenders with the pid probe).
const LIVE_WINDOW_SECS: i64 = 15 * 60;

// ── M3: tri-state liveness (agent-situational-awareness handoff) ──────────────
//
// Liveness is NOT binary. The dangerous bug is treating a network-PARTITIONED
// agent (alive, still editing files, just off the bus) as dead and reaping its
// claims. Three states, biased toward "might still be running":
//   • connected    — the broker holds a live session AND (same host) the owner
//                     pid passes a signal-0 probe AND `last_active` is fresh.
//   • disconnected — the broker lost it (Last-Will fired / a clean stop / an
//                     eventloop error), so it's off the bus — but it MAY still be
//                     a live split brain. Its claims MUST NOT be reaped.
//   • dead         — CONFIRMED gone: a same-host owner-pid signal-0 probe fails.
// A disconnected session whose pid is still alive is a SPLIT BRAIN (same host,
// confirmed still running). A disconnected session we cannot probe (cross host,
// no local pid) is `disconnected (unknown)` — never auto-reaped, because
// cross-host death is unconfirmable. Only `Dead` ever reaps claims.

/// Why a session is disconnected — drives the ambient-note warning and whether we
/// can ever escalate it to `Dead`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectKind {
    /// Same host, the owner pid is STILL ALIVE: a confirmed live split brain that
    /// merely lost the bus. It may still be editing files — treat its claims as
    /// live. Never reaped (the pid is alive).
    SplitBrain,
    /// Cross host / unprobeable: we cannot confirm death, so it stays disconnected
    /// indefinitely rather than being auto-reaped.
    Unknown,
}

/// Tri-state liveness of a coding session (M3). `Dead` is the ONLY state that
/// authorizes reaping a session's advisory claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Connected,
    Disconnected(DisconnectKind),
    Dead,
}

/// Classify a session's liveness from the broker's connection view and a same-host
/// pid probe (M3). Pure over its inputs so it is unit-testable without a broker.
///
/// - `broker_connected`: the broker's OWN view (`code_sessions.connected`): `Some(true)`
///   holding a live MQTT session, `Some(false)` lost it (LWT fired / clean stop /
///   partition), `None` unknown (no beacon yet).
/// - `pid_probe`: `Some(true/false)` when the session has a same-host owner pid we
///   can signal-0 (true = alive, false = the process is gone → CONFIRMED dead);
///   `None` when there is no local pid to probe (cross host / no membership) — death
///   is then UNCONFIRMABLE.
///
/// Safety invariant: `Dead` is returned ONLY on a confirmed same-host pid death.
/// A disconnected session with a live pid is `Disconnected(SplitBrain)`; a
/// disconnected session we cannot probe is `Disconnected(Unknown)`. Neither reaps.
pub fn classify_liveness(broker_connected: Option<bool>, pid_probe: Option<bool>) -> Liveness {
    match pid_probe {
        // Same host, the process is GONE — the one signal that confirms death.
        Some(false) => Liveness::Dead,
        // Same host, the process is ALIVE — never dead. Off the bus → split brain.
        Some(true) => match broker_connected {
            Some(false) => Liveness::Disconnected(DisconnectKind::SplitBrain),
            _ => Liveness::Connected,
        },
        // No local pid to probe: death is unconfirmable (cross host / reaped
        // membership). Connected requires POSITIVE evidence of life — only a
        // broker view of connected=true yields Connected here. No signal at all
        // (no pid, no broker view — e.g. a legacy/beacon-less or long-idle
        // session) is Disconnected(Unknown), NOT Connected: absence of evidence
        // is not evidence of life. (Still never Dead — death stays unconfirmable
        // without a pid, so its claims are never reaped.)
        None => match broker_connected {
            Some(true) => Liveness::Connected,
            _ => Liveness::Disconnected(DisconnectKind::Unknown),
        },
    }
}

/// One live sibling sharing this session's workdir (SA2 roster entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSibling {
    /// The sibling session id (`code-<id>`).
    pub session: String,
    /// The obs noun it runs under (`claude-code` | `codex`) — shown in the line.
    pub agent_noun: String,
    /// SI1 (sibling-intent): when this sibling was last active, RFC3339 — the
    /// FRESHER of its `code_sessions.last_active` and the projection's
    /// `code_session_stats.updated_at` (its most-recent obs event). Lets a viewer
    /// judge alive-vs-stranded before touching the sibling's WIP.
    pub last_active: String,
    /// SI2 (sibling-intent): what this sibling is currently working on, as
    /// `(text, status)` selected from its `code_session_tasks` projection — the
    /// `in_progress` item if any, else the most-recently-updated item. `None` when
    /// the session has no projected task list yet (e.g. an opencode sibling, which
    /// emits no todo event — honestly absent, not a fake empty list). `status` is
    /// one of `todo|in_progress|done`.
    pub current_task: Option<(String, String)>,
    /// M2 (situational-awareness): the session's BASELINE intent — the launch task
    /// string (`lanius code spawn/deliver`) or the first user prompt of an
    /// interactive session, from `code_sessions.intent`. Surfaced in the ambient
    /// note when the session has no refined todo (`current_task`), so a harness that
    /// emits no todo (codex/opencode) still shows what it was asked to do. `None`
    /// when no intent was ever recorded → the note reads "(no stated intent)".
    pub intent: Option<String>,
    /// M3 (situational-awareness): tri-state liveness. A `Disconnected` sibling is
    /// kept in the roster (flagged in the note) precisely because it may be a live
    /// split brain — its claims must be treated as live, never silently dropped.
    pub liveness: Liveness,
}

/// List the LIVE sibling sessions sharing `viewer`'s canonical workdir (SA2). A
/// sibling is any OTHER `code_sessions` row whose workdir canonicalizes to the
/// same path AND that is live by the honest definition above. `viewer` is
/// excluded (a session is never its own sibling). Ordered most-recently-active
/// first so the capped injection line surfaces the most relevant peers. Returns an
/// empty list (never an error to the caller) so a quiet/solo turn stays quiet.
pub fn live_siblings(root: &Root, viewer: &str, viewer_workdir: &str) -> Vec<LiveSibling> {
    let want = canon_str(viewer_workdir);
    let Ok(conn) = crate::db::open(root) else {
        return Vec::new();
    };
    if crate::db::init_schema(&conn).is_err() {
        return Vec::new();
    }
    // Candidate rows: every recorded session except the viewer. We filter by
    // canonical workdir + liveness in Rust (canonicalization and the pid probe
    // are not SQL). Pull owner_pid from the room membership (may be absent).
    let mut stmt = match conn.prepare(
        "SELECT s.elanus_session, s.agent_noun, s.workdir, s.last_active, m.owner_pid,
                s.intent, s.connected
           FROM code_sessions s
           LEFT JOIN code_room_members m ON m.session = s.elanus_session
          WHERE s.elanus_session <> ?1",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([viewer], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<i32>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<i64>>(6)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };
    let now = chrono_now_secs();
    let mut out: Vec<(i64, LiveSibling)> = Vec::new();
    for row in rows.flatten() {
        let (session, agent_noun, workdir, last_active, owner_pid, intent, connected) = row;
        if canon_str(&workdir) != want {
            continue; // a different checkout — not a sibling here
        }
        // SI1: a session's recency is the FRESHER of its `code_sessions.last_active`
        // (bumped on resume/obs-publish) and the projection's per-session
        // `code_session_stats.updated_at` (its most-recent obs event) — so a
        // long-running session whose record was not re-bumped still reads as live
        // off its event stream. Computed in Rust (the stats table may not exist).
        let stats_upd = session_stats_updated_at(&conn, &session);
        let last_active = fresher_iso(&last_active, stats_upd.as_deref());
        let last_secs = iso_to_secs(&last_active);
        let fresh = match last_secs {
            Some(t) => now.saturating_sub(t) <= LIVE_WINDOW_SECS,
            None => false, // an unparseable timestamp is not trusted as live
        };
        // M3: tri-state liveness. A same-host owner pid gives a real probe; no
        // membership row → no local pid to probe (`None`, unconfirmable). The
        // broker's own connection view (`connected`) rides the retained status
        // topic the beacon publishes / the Last-Will fires.
        let broker_connected = connected.map(|c| c != 0);
        let pid_probe = owner_pid.map(pid_alive);
        let liveness = classify_liveness(broker_connected, pid_probe);
        // A confirmed-dead session never haunts the injection.
        if liveness == Liveness::Dead {
            continue;
        }
        let disconnected = matches!(liveness, Liveness::Disconnected(_));
        // Age-out rule (unchanged for the common case): a session past the freshness
        // window drops from the roster — EXCEPT a broker-disconnected one, which we
        // KEEP and flag, because a partitioned split brain that has been off the bus
        // a while is exactly the claim-collision hazard peers must be warned about
        // (M3: disconnected ≠ aged-out; never silently drop a possible split brain).
        if !fresh && !disconnected {
            continue; // stale but still on the bus → aged out, as before
        }
        // SI2: enrich with the sibling's current task (one cheap extra SELECT on
        // the open connection). Reuses the standalone selection logic so the note
        // and a `whose`/`sessions` CLI verb all agree on which item is "current".
        let current_task = current_task_on(&conn, &session);
        out.push((
            last_secs.unwrap_or(0),
            LiveSibling {
                session,
                agent_noun,
                last_active,
                current_task,
                intent: intent.filter(|s| !s.trim().is_empty()),
                liveness,
            },
        ));
    }
    // Most-recently-active first.
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.session.cmp(&b.1.session)));
    out.into_iter().map(|(_, s)| s).collect()
}

/// Canonicalize a workdir string to a comparable absolute path string, falling
/// back to the raw string when canonicalize fails (a removed dir) so two sessions
/// recorded with the same raw workdir still compare equal.
fn canon_str(workdir: &str) -> String {
    std::fs::canonicalize(workdir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| workdir.to_string())
}

/// Current unix time in seconds (best-effort; 0 if the clock is before the epoch).
fn chrono_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse one of our `strftime('%Y-%m-%dT%H:%M:%fZ')` timestamps to unix seconds.
/// None on any parse failure (treated as "not live" by the caller).
fn iso_to_secs(ts: &str) -> Option<i64> {
    // The stored format is RFC3339-ish (`2026-06-22T12:34:56.789Z`). Parse with
    // chrono, which the projection already depends on.
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp())
}

/// SI1: read the projection's `code_session_stats.updated_at` (a session's
/// most-recent obs event time) on an EXISTING connection. None on any error,
/// including the projection table not existing yet — the caller then falls back to
/// `code_sessions.last_active` alone ("compute the max of the two in Rust").
fn session_stats_updated_at(conn: &rusqlite::Connection, session: &str) -> Option<String> {
    conn.query_row(
        "SELECT updated_at FROM code_session_stats WHERE elanus_session = ?1",
        [session],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// SI1: the fresher (later) of two RFC3339 timestamps. `b` is optional; when it is
/// absent or unparseable, `a` wins; when `a` is unparseable but `b` parses, `b`
/// wins. Folds `code_sessions.last_active` with the projection's `updated_at`
/// without a SQL join (the stats table may not exist on this connection).
fn fresher_iso(a: &str, b: Option<&str>) -> String {
    match b {
        None => a.to_string(),
        Some(b) => match (iso_to_secs(a), iso_to_secs(b)) {
            (Some(ta), Some(tb)) => {
                if tb > ta {
                    b.to_string()
                } else {
                    a.to_string()
                }
            }
            (None, Some(_)) => b.to_string(),
            _ => a.to_string(),
        },
    }
}

/// SI2 (sibling-intent): pick the task to DISPLAY for a session from its
/// `code_session_tasks` rows on an EXISTING connection — the `in_progress` item if
/// any (the most-recently-updated when several), else the most-recently-`updated_at`
/// item. Returns `(text, status)`. None when the session has no projected task list
/// (the projection has folded no todo event for it yet), the table doesn't exist, or
/// on any query error — best-effort, never an error to the caller.
fn current_task_on(conn: &rusqlite::Connection, session: &str) -> Option<(String, String)> {
    conn.query_row(
        "SELECT text, status FROM code_session_tasks
          WHERE elanus_session = ?1
          ORDER BY (status = 'in_progress') DESC, updated_at DESC, item_id ASC
          LIMIT 1",
        [session],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .optional()
    .ok()
    .flatten()
}

/// SI2 (sibling-intent): a session's CURRENT task — the `in_progress` item if any,
/// else the most-recently-updated item — as `(text, status)`. Standalone (opens its
/// own connection); `live_siblings`/`whose_path` use `current_task_on` on their
/// already-open connection. The public accessor the SI2 projection tests assert
/// through and the intended `whose`/`sessions` CLI surface; `allow(dead_code)` until
/// that CLI wiring lands. None when there is no projected task list for the session.
#[allow(dead_code)]
pub fn current_task(root: &Root, session: &str) -> Option<(String, String)> {
    let conn = crate::db::open(root).ok()?;
    let _ = crate::db::init_schema(&conn);
    current_task_on(&conn, session)
}

/// Read a durable record by lanius session id. None if there is no such session
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

// ── M2: baseline session intent (agent-situational-awareness handoff) ─────────
//
// The launch task string (`lanius code spawn/deliver <tool> "<task>"`) or the
// first user prompt of an interactive session is the session's BASELINE intent —
// what it was ASKED to do, known even when the harness emits no todo. SI2 todos
// REFINE it; they no longer GATE it. Stored on the durable record so a codex or
// opencode session that never produces a todo still shows a stated intent in the
// ambient sibling note and `lanius code sessions`.

/// Record a session's baseline intent, unconditionally (the launch path calls this
/// once at launch with the launch task). A stub record is created if none exists
/// yet — the native-id upsert later fills the rest (COALESCE preserves intent).
/// Blank intent is ignored (never clobbers a real one with emptiness). Best-effort.
pub fn set_intent(root: &Root, session: &str, intent: &str) -> Result<()> {
    let intent = intent.trim();
    if intent.is_empty() {
        return Ok(());
    }
    let conn = crate::db::open(root).context("opening the ledger to set the intent")?;
    crate::db::init_schema(&conn)?;
    let n = conn.execute(
        "UPDATE code_sessions SET intent = ?2 WHERE elanus_session = ?1",
        rusqlite::params![session, intent],
    )?;
    if n == 0 {
        conn.execute(
            "INSERT INTO code_sessions
               (elanus_session, native_session, tool, agent_noun, workdir, intent)
             VALUES (?1, '', '', '', '', ?2)
             ON CONFLICT(elanus_session) DO UPDATE SET intent = excluded.intent",
            rusqlite::params![session, intent],
        )?;
    }
    Ok(())
}

/// Record a session's intent ONLY if it has none yet (the interactive first-user-
/// prompt path: the launch carried no task, so the first prompt becomes the
/// baseline — but a later prompt must not overwrite it). Best-effort.
pub fn set_intent_if_absent(root: &Root, session: &str, intent: &str) -> Result<()> {
    let conn = crate::db::open(root).context("opening the ledger to seed the intent")?;
    crate::db::init_schema(&conn)?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT intent FROM code_sessions WHERE elanus_session = ?1",
            [session],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    if existing.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()) {
        return Ok(()); // already has a baseline intent — the launch task wins
    }
    set_intent(root, session, intent)
}

/// Read a session's baseline intent (M2). None when none was recorded.
pub fn get_intent(root: &Root, session: &str) -> Option<String> {
    let conn = crate::db::open(root).ok()?;
    let _ = crate::db::init_schema(&conn);
    conn.query_row(
        "SELECT intent FROM code_sessions WHERE elanus_session = ?1",
        [session],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .filter(|s| !s.trim().is_empty())
}

// ── M4: session ↔ worktree/branch ↔ outcome ledger (situational-awareness) ────
//
// A past session's git artifacts (a branch, a worktree) had no lanius record of
// who made them, why, or their terminal status — so accounting for them meant git
// archaeology (Incident B). M4 records the branch a session works on (the workdir
// is already stored — that IS the worktree) and derives a terminal OUTCOME from
// `git branch --merged` + the tri-state liveness. `lanius code sitrep` renders one
// view of every session and loose worktree so "account for all the other work" is
// a query, not archaeology.

/// A session's terminal outcome (M4), derived from git + liveness at display time
/// (never stored — it changes as the branch merges / the process dies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The session is still live (connected, or a disconnected split brain that may
    /// still be editing) — its work is in flight, not terminal.
    Active,
    /// The session is dead AND its branch's work is in the default branch, with a
    /// clean worktree: the artifacts are safe to remove.
    Merged,
    /// The session is dead, its branch is NOT merged, and it left nothing to save
    /// (no unmerged commits, a clean worktree) — an empty, abandonable leftover.
    Abandoned,
    /// The session is dead but its worktree is dirty OR it has unmerged commits:
    /// there is UNSHIPPED work stranded here — do NOT remove without rescuing it.
    WipStranded,
}

impl Outcome {
    /// A short, human label for the sitrep view.
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Active => "active",
            Outcome::Merged => "merged — safe to remove",
            Outcome::Abandoned => "abandoned — safe to remove",
            Outcome::WipStranded => "wip-stranded — unshipped work, do not remove",
        }
    }
}

/// Derive a session/worktree's terminal outcome (M4). Pure over its inputs so it is
/// unit-testable without a git repo. Safety-biased like the liveness classifier: a
/// non-`Dead` session is always `Active` (never declared removable while it might be
/// running), and unshipped work (dirty tree or unmerged commits) always wins over
/// "merged/safe" so a dirty merged worktree is still flagged `WipStranded`.
///
/// - `liveness`: the session's tri-state liveness (`classify_liveness`). A loose
///   worktree with no owning session is classified as `Dead` (nothing is running it).
/// - `branch_merged`: is the branch's work already in the default branch?
/// - `has_unmerged_commits`: does the branch carry commits not in the default branch?
/// - `dirty`: does the worktree have uncommitted changes?
pub fn classify_outcome(
    liveness: Liveness,
    branch_merged: bool,
    has_unmerged_commits: bool,
    dirty: bool,
) -> Outcome {
    if liveness != Liveness::Dead {
        return Outcome::Active;
    }
    // Unshipped work wins over "merged": a dirty or ahead worktree is never "safe".
    if dirty || has_unmerged_commits {
        return Outcome::WipStranded;
    }
    if branch_merged {
        return Outcome::Merged;
    }
    Outcome::Abandoned
}

/// Record the git branch a session works on (M4). Mirrors `set_intent`: an UPDATE,
/// falling back to a stub INSERT if the record isn't observed yet. Blank branch is
/// ignored. Best-effort — a ledger hiccup never breaks the launch.
pub fn set_branch(root: &Root, session: &str, branch: &str) -> Result<()> {
    let branch = branch.trim();
    if branch.is_empty() {
        return Ok(());
    }
    let conn = crate::db::open(root).context("opening the ledger to set the branch")?;
    crate::db::init_schema(&conn)?;
    let n = conn.execute(
        "UPDATE code_sessions SET branch = ?2 WHERE elanus_session = ?1",
        rusqlite::params![session, branch],
    )?;
    if n == 0 {
        conn.execute(
            "INSERT INTO code_sessions
               (elanus_session, native_session, tool, agent_noun, workdir, branch)
             VALUES (?1, '', '', '', '', ?2)
             ON CONFLICT(elanus_session) DO UPDATE SET branch = excluded.branch",
            rusqlite::params![session, branch],
        )?;
    }
    Ok(())
}

/// One coding session in the sitrep view (M4): the durable record enriched with the
/// tri-state liveness. The git-derived OUTCOME is computed by the CLI (it needs to
/// run git in the workdir); this carries everything the ledger knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitrepSession {
    /// The lanius session id (`code-<id>`).
    pub session: String,
    /// The obs noun / tool the session ran under (`claude-code` | `codex` | …).
    pub agent_noun: String,
    /// The binary that ran it (`claude` | `codex` | …), for the resume hint.
    pub tool: String,
    /// Absolute worktree directory the session ran in.
    pub workdir: String,
    /// The git branch it works on, if recorded (M4). None outside a git repo.
    pub branch: Option<String>,
    /// The session's baseline intent (M2), if recorded — what it was asked to do.
    pub intent: Option<String>,
    /// When it was last active, RFC3339 (the FRESHER of `last_active` and the
    /// projection's `updated_at`, same recency rule as `live_siblings`).
    pub last_active: String,
    /// The session's tri-state liveness (M3) — feeds the outcome classification.
    pub liveness: Liveness,
}

/// List EVERY recorded coding session for the sitrep view (M4), each with its
/// tri-state liveness computed the same way `live_siblings` does (broker view +
/// same-host pid probe). Unlike `live_siblings` this does NOT filter by workdir or
/// drop dead sessions — the whole point of sitrep is to account for dead sessions'
/// leftover artifacts. Stub records with no workdir AND no branch (intent-only
/// placeholders) are skipped as noise. Most-recently-active first. Best-effort:
/// returns an empty list on any ledger error.
pub fn sitrep_sessions(root: &Root) -> Vec<SitrepSession> {
    let Ok(conn) = crate::db::open(root) else {
        return Vec::new();
    };
    if crate::db::init_schema(&conn).is_err() {
        return Vec::new();
    }
    let mut stmt = match conn.prepare(
        "SELECT s.elanus_session, s.agent_noun, s.tool, s.workdir, s.branch,
                s.intent, s.last_active, m.owner_pid, s.connected
           FROM code_sessions s
           LEFT JOIN code_room_members m ON m.session = s.elanus_session",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, Option<i32>>(7)?,
            r.get::<_, Option<i64>>(8)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<(i64, SitrepSession)> = Vec::new();
    for row in rows.flatten() {
        let (session, agent_noun, tool, workdir, branch, intent, last_active, owner_pid, connected) =
            row;
        // Skip intent/branch-only stubs that never became a real session (no place
        // on disk to account for).
        if workdir.trim().is_empty() && branch.as_deref().unwrap_or("").trim().is_empty() {
            continue;
        }
        let stats_upd = session_stats_updated_at(&conn, &session);
        let last_active = fresher_iso(&last_active, stats_upd.as_deref());
        let last_secs = iso_to_secs(&last_active).unwrap_or(0);
        let broker_connected = connected.map(|c| c != 0);
        let pid_probe = owner_pid.map(pid_alive);
        let liveness = classify_liveness(broker_connected, pid_probe);
        out.push((
            last_secs,
            SitrepSession {
                session,
                agent_noun,
                tool,
                workdir,
                branch: branch.filter(|s| !s.trim().is_empty()),
                intent: intent.filter(|s| !s.trim().is_empty()),
                last_active,
                liveness,
            },
        ));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.session.cmp(&b.1.session)));
    out.into_iter().map(|(_, s)| s).collect()
}

/// Classify ONE named session's tri-state liveness (M3/M5) from the ledger: its
/// broker connection view (`code_sessions.connected`) and a same-host owner-pid
/// probe (from its room membership). The `ask` liveness pre-check and `watch` use
/// this to fail fast on a dead target instead of blocking to timeout. None when the
/// session has no record at all (never launched / unknown id). A session with a
/// record but no membership pid is probed as `None` → the broker view alone decides
/// (so an unknown-but-connected session still reads live, never wrongly dead).
pub fn session_liveness(root: &Root, session: &str) -> Option<Liveness> {
    let conn = crate::db::open(root).ok()?;
    let _ = crate::db::init_schema(&conn);
    let row: Option<(Option<i64>, Option<i32>)> = conn
        .query_row(
            "SELECT s.connected, m.owner_pid
               FROM code_sessions s
               LEFT JOIN code_room_members m ON m.session = s.elanus_session
              WHERE s.elanus_session = ?1",
            [session],
            |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i32>>(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    let (connected, owner_pid) = row?;
    let broker_connected = connected.map(|c| c != 0);
    let pid_probe = owner_pid.map(pid_alive);
    Some(classify_liveness(broker_connected, pid_probe))
}

/// Record the broker's connection view for a session (M3). The liveness beacon
/// calls this: `true` when it holds a live MQTT session, `false` when the bus
/// connection is lost (a clean stop, or the eventloop errored — a partition that
/// makes this a possible split brain). This is the `connected` half of tri-state
/// liveness; the pid probe supplies the `dead` half. Best-effort — a ledger hiccup
/// never breaks the session (it just falls back to the pid/freshness signals).
pub fn set_connected(root: &Root, session: &str, connected: bool) -> Result<()> {
    let conn = crate::db::open(root).context("opening the ledger to set the connection view")?;
    crate::db::init_schema(&conn)?;
    conn.execute(
        "UPDATE code_sessions
            SET connected = ?2,
                conn_updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE elanus_session = ?1",
        rusqlite::params![session, connected as i32],
    )?;
    Ok(())
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
pub fn claim_delivery_key(root: &Root, key: &str, session: &str, event_id: i64) -> Result<bool> {
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
// caller (the `lanius code inbox` CLI) derives `<noun>`/`<session>` from the
// process env the launcher set (LANIUS_CODE_AGENT / LANIUS_CODE_SESSION), never
// from an argument — so a session can never name another session's inbox, and
// the read is own-inbox-only BY CONSTRUCTION. The bus token's subscribe scope is
// untouched (still empty): the read authority is the kernel-side query, gated by
// the env-derived identity, exactly as `lanius code hook` publishes as itself.

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
    /// The delivery's `events.priority` (C3 — agent-comms). A higher number is
    /// more urgent; it drives the inbox block's injection vector (HIGH-priority
    /// unseen mail reaches the model mid-cycle on Claude Code). Default 0.
    pub priority: i32,
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
                (s.event_id IS NOT NULL) AS seen, COALESCE(e.priority, 0) AS priority
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
                r.get::<_, i32>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for (event_id, payload, from, correlation, state, created_at, seen, priority) in rows {
        if unseen_only && seen {
            continue;
        }
        let pv: serde_json::Value =
            serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
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
            priority,
        });
    }
    Ok(items)
}

/// Mark a set of the session's own inbox deliveries as seen (idempotent). Called
/// after `lanius code inbox` lists them, so a second pull does not re-surface the
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

/// C3 (agent-comms) — claim the unseen, HIGH-priority inbox messages that have
/// not yet been handed to this session MID-CYCLE, recording each as delivered so
/// it is not re-injected on the next tool call. A message qualifies when it is
/// unseen (not yet pulled via `code inbox`) AND its `events.priority >= threshold`.
/// Returns the newly-claimed items (newest last). This MUTATES the dedup table —
/// call it only from the emitting hook arm, never as a pure read, or a message
/// would be marked delivered without being shown.
///
/// This deliberately does NOT mark the messages `seen`: the agent has not pulled
/// them, so they still count in the next-turn inbox block; this only suppresses the
/// louder mid-cycle re-injection of the SAME message every tool call. Mirrors
/// `context_store::take_pending_mid_cycle` for blocks, but keyed by the immutable
/// event id (mail is not edited in place, so one delivery per message).
pub fn take_pending_mid_cycle_mail(
    root: &Root,
    agent_noun: &str,
    session: &str,
    threshold: i32,
) -> Result<Vec<InboxItem>> {
    if !is_session_principal(session) {
        return Ok(Vec::new());
    }
    let unseen = inbox_for_session(root, agent_noun, session, true)?;
    let conn = crate::db::open(root).context("opening the ledger for mid-cycle mail")?;
    crate::db::init_schema(&conn)?;
    let mut out = Vec::new();
    for item in unseen {
        if item.priority < threshold {
            continue; // normal mail rides the next-turn inbox block only
        }
        // Already handed mid-cycle? (per (session, event_id)) → skip.
        let already: bool = conn
            .query_row(
                "SELECT 1 FROM code_mail_delivered WHERE session = ?1 AND event_id = ?2",
                rusqlite::params![session, item.event_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if already {
            continue;
        }
        conn.execute(
            "INSERT INTO code_mail_delivered (session, event_id) VALUES (?1, ?2)
             ON CONFLICT(session, event_id) DO NOTHING",
            rusqlite::params![session, item.event_id],
        )?;
        out.push(item);
    }
    Ok(out)
}

/// The owner (agent noun) a session's memory `note` block is stored under. The
/// note block is session-scoped and owned by the session's agent noun (the coding
/// agent's identity the launcher recorded). Read it off the durable record; fall
/// back to a stable generic owner when the record is not yet observed, so a note
/// set before SessionStart still round-trips by the same key. The agent noun is a
/// name (no slash/whitespace), valid as a block owner.
fn note_owner(root: &Root, session: &str) -> String {
    read_record(root, session)
        .ok()
        .flatten()
        .map(|r| r.agent_noun)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "code-agent".to_string())
}

/// Set (or replace) a session's memory note — the small editable block a planner
/// leaves a worker, surfaced by the per-turn injection. An empty note clears it (a
/// deliberate way to remove a stale reminder). The session must be a valid `code-*`
/// id.
///
/// M2 decision 5 (memory-blocks handoff): the note IS a well-known session-scope
/// `note` block in the `context_blocks` substrate — this is a thin alias that
/// writes that block. `lanius code note` keeps working; the next-turn injection
/// reads the block, not a separate `code_notes` path. (`code_notes` remains in the
/// schema as legacy; nothing live reads it anymore.)
pub fn set_note(root: &Root, session: &str, note: &str) -> Result<()> {
    if !is_session_principal(session) {
        bail!("note session {session:?} is not a valid code-* identity name");
    }
    let conn = crate::db::open(root).context("opening the ledger to set the note")?;
    crate::db::init_schema(&conn)?;
    let owner = note_owner(root, session);
    crate::context_store::set_session_note(&conn, "default", &owner, session, note)
}

/// Read a session's memory note, if one is set. None when there is no note (the
/// per-turn injection omits the note line in that case). Reads the `note` block
/// (M2 decision 5), the same key `set_note` writes.
pub fn get_note(root: &Root, session: &str) -> Result<Option<String>> {
    if !is_session_principal(session) {
        return Ok(None);
    }
    let conn = crate::db::open(root).context("opening the ledger to read the note")?;
    crate::db::init_schema(&conn)?;
    let owner = note_owner(root, session);
    crate::context_store::get_session_note(&conn, &owner, session)
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

// ── SI4: change attribution — who owns this path? ─────────────────────────────
//
// docs/handoffs/sibling-intent.md SI4 (`whose-change`). Resolve a path to the
// coding session that last claimed it, freshest-claim-wins, off `code_claims` —
// which carries claims from ALL THREE harnesses via each one's OWN write-tool
// events (`auto_claim_write`: claude's Write/Edit hook, codex `file_change`,
// opencode `edit`/`write`), plus a manual `lanius code claim`. Advisory, gates
// nothing: it answers "which of these dirty files are mine, and who owns the
// rest?" — the exact question the motivating incident got wrong by hand.

/// Who owns a path: the session that holds the freshest `code_claims` claim on it,
/// plus that session's agent noun, last-active recency, and current task — the
/// answer `lanius code whose <path>` renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    /// The owning session id (`code-<id>`) — who is/was editing the path.
    pub session: String,
    /// The obs noun that session runs under (`claude-code` | `codex` | `opencode`).
    pub agent_noun: String,
    /// When that session was last active, RFC3339 — the FRESHER of its
    /// `code_sessions.last_active` and the projection's `code_session_stats.updated_at`
    /// (same recency rule as `live_siblings`). Empty when no record/stats exist.
    pub last_active: String,
    /// That session's current task TEXT (SI2), if it has a projected task list. None
    /// for a harness that emits no todo event (opencode) — honestly absent.
    pub current_task: Option<String>,
}

/// Resolve a lookup path to the canonical, absolute form claims are STORED in
/// (`canonicalize_claim_path` in codeagent.rs and the SI3 fs-touch claim both store
/// the canonicalized absolute path): a relative path is joined against the process
/// CWD, then symlink-resolved best-effort (falling back to the lexical absolute when
/// it no longer exists). This makes a `whose_path` lookup match an auto-claim
/// recorded by the camera, which keys on the canonical path.
fn canon_claim_lookup(path: &str) -> String {
    let p = path.trim();
    let pb = PathBuf::from(p);
    let abs = if pb.is_absolute() {
        pb
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(&pb)
    } else {
        pb
    };
    std::fs::canonicalize(&abs)
        .unwrap_or(abs)
        .to_string_lossy()
        .into_owned()
}

/// The session holding the freshest claim on `path` (exact path match), or None.
/// Freshest-wins by `created_at` (the SI3 fs-touch and Write/Edit auto-claims
/// refresh `created_at` on each write, so the live editor wins over a stale one).
fn freshest_claim_holder(conn: &rusqlite::Connection, path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    conn.query_row(
        "SELECT session FROM code_claims WHERE path = ?1
          ORDER BY created_at DESC, session ASC LIMIT 1",
        [path],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// A session's `(agent_noun, last_active)` for attribution: the noun off its durable
/// record and the FRESHER of `code_sessions.last_active` and the projection's
/// `code_session_stats.updated_at` (the same recency rule `live_siblings` uses).
/// Empty strings when the session has no record (a claim whose session was never
/// recorded — still attributed by id, just without enrichment).
fn session_identity(conn: &rusqlite::Connection, session: &str) -> (String, String) {
    let rec: Option<(String, String)> = conn
        .query_row(
            "SELECT agent_noun, last_active FROM code_sessions WHERE elanus_session = ?1",
            [session],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                ))
            },
        )
        .optional()
        .ok()
        .flatten();
    let (agent_noun, last_active) = rec.unwrap_or_default();
    let last_active = fresher_iso(
        &last_active,
        session_stats_updated_at(conn, session).as_deref(),
    );
    (agent_noun, last_active)
}

/// SI4 (sibling-intent): which session owns `path`? Resolves the path to its owning
/// coding session via the freshest `code_claims` claim — covering claude's hook
/// auto-claims, manual `claim`s, AND the SI3 projection fs-touch auto-claims for
/// hookless codex/opencode — then fills the owner's agent noun, last-active recency,
/// and current task. The lookup path is canonicalized the same way claims are stored
/// (absolute, symlink-resolved, relative resolved against the CWD) so it matches a
/// camera-recorded claim; a verbatim string is also tried as a fallback for a manual
/// claim recorded with no workdir base. None when no claim matches.
pub fn whose_path(root: &Root, path: &str) -> Option<Attribution> {
    let Ok(conn) = crate::db::open(root) else {
        return None;
    };
    if crate::db::init_schema(&conn).is_err() {
        return None;
    }
    let canon = canon_claim_lookup(path);
    let session = freshest_claim_holder(&conn, &canon)
        .or_else(|| freshest_claim_holder(&conn, path.trim()))?;
    let (agent_noun, last_active) = session_identity(&conn, &session);
    let current_task = current_task_on(&conn, &session).map(|(text, _status)| text);
    Some(Attribution {
        session,
        agent_noun,
        last_active,
        current_task,
    })
}

/// One recent message seen on a shared channel (C4 — agent-comms): a room's
/// (`in/group/<id>`) traffic, surfaced advisory in the `channel:<id>` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMsg {
    /// The broker-verified sender, if recorded.
    pub from: Option<String>,
    /// The message text (the delivery's prompt/text field).
    pub message: String,
    /// When it was recorded.
    pub created_at: String,
}

/// C4 (agent-comms) — the most recent `recent_n` messages on a room's shared
/// channel (`in/group/<id>`), newest last. Advisory: this is the channel block's
/// source. The `room` is the session's OWN room (derived from its record/workdir
/// by the caller), never an argument a peer supplies — a session can only ever see
/// the channel of a room it belongs to. An empty room or zero bound yields nothing
/// (a session not in any room sees no channel). Bounded by `recent_n` so a busy
/// channel cannot flood the turn. Best-effort: a ledger error yields an empty list.
pub fn room_recent(root: &Root, room: &str, recent_n: usize) -> Result<Vec<ChannelMsg>> {
    if room.is_empty() || recent_n == 0 {
        return Ok(Vec::new());
    }
    let topic = format!("in/group/{}", crate::topic::encode_segment(room));
    let conn = crate::db::open(root).context("opening the ledger for the channel")?;
    crate::db::init_schema(&conn)?;
    let mut stmt = conn.prepare(
        "SELECT e.sender, COALESCE(e.payload,''), e.created_at
           FROM events e
          WHERE e.type = ?1
          ORDER BY e.id DESC
          LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![topic, recent_n as i64], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // The query returns newest first (bounded); reverse to newest-LAST for the
    // chronological render the injection uses elsewhere.
    let mut out: Vec<ChannelMsg> = rows
        .into_iter()
        .map(|(from, payload, created_at)| {
            let pv: serde_json::Value =
                serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
            let message = crate::codeagent::delivery_message(&pv).unwrap_or_default();
            ChannelMsg {
                from,
                message,
                created_at,
            }
        })
        .collect();
    out.reverse();
    Ok(out)
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
    // Join the broker's connection view so the reap decision is tri-state: a
    // DISCONNECTED session (broker lost it) whose pid is still alive is a possible
    // SPLIT BRAIN — classify_liveness returns Disconnected(SplitBrain), NOT Dead,
    // so we leave its claims alone. Only a confirmed same-host pid death reaps.
    let members: Vec<(String, String, i32, Option<i64>)> = {
        let Ok(mut stmt) = conn.prepare(
            "SELECT m.room, m.session, m.owner_pid, s.connected
               FROM code_room_members m
               LEFT JOIN code_sessions s ON s.elanus_session = m.session",
        ) else {
            return reaped;
        };
        let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i32>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        }) else {
            return reaped;
        };
        rows.filter_map(|r| r.ok()).collect()
    };
    for (room, session, owner_pid, connected) in members {
        // A membership row always carries a same-host owner pid, so the probe is
        // authoritative. REAP ONLY ON CONFIRMED DEAD (pid gone). A disconnected
        // split brain (pid alive) keeps its claims — this is the M3 safety rule.
        let broker_connected = connected.map(|c| c != 0);
        let liveness = classify_liveness(broker_connected, Some(pid_alive(owner_pid)));
        if liveness == Liveness::Dead {
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

/// SI1 (sibling-intent): bump a session's `code_sessions.last_active` to now from
/// the obs-publish path (codeagent.rs calls this each time the session emits
/// telemetry), so a long-running session stays "live" between resumes rather than
/// reading as stale off a `last_active` that only `upsert_record`/`touch_record`
/// bump on resume. Same effect as `touch_record`; named for the obs-path caller.
/// Best-effort: a ledger error never breaks the session.
pub fn bump_last_active(root: &Root, session: &str) -> Result<()> {
    touch_record(root, session)
}

// ── Detached-spawn edges (cross-harness-death M1/M2) ──────────────────────────
//
// A durable record of an `lanius code spawn` detached worker: who it must report
// to, the correlation the completion threads on, and its wrapper pid. The worker's
// own completion (`emit_completion_delivery`) and the daemon reaper
// (`reap_dead_spawn_edges`) both CLAIM the edge before mailing — an atomic
// `UPDATE ... WHERE settled_at IS NULL` — so whichever fires first is the sole
// producer of the spawner's completion mail; the loser sees 0 rows and stays
// silent. This is the idempotency that makes a slow-worker/reaper race safe.

/// A detached spawn edge that has not yet reported a completion.
#[derive(Debug, Clone)]
pub struct SpawnEdge {
    pub worker_session: String,
    pub spawner: String,
    pub correlation: Option<String>,
    pub worker_pid: i32,
}

/// The outcome of trying to claim a spawn edge's settle. `Claimed` means THIS
/// caller won and must produce the completion mail; `AlreadySettled` means the
/// other producer beat it (stay silent); `NoEdge` means no edge was recorded (a
/// legacy/non-spawn completion — the caller mails unconditionally, there is no
/// reaper to double it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleClaim {
    Claimed,
    AlreadySettled,
    NoEdge,
}

/// Record the durable spawn edge for a detached worker, before it is left to run.
/// Idempotent on the worker session (a re-record replaces the row) — the worker
/// session is minted unique per spawn, so this only ever writes one live edge.
pub fn record_spawn_edge(
    root: &Root,
    worker_session: &str,
    spawner: &str,
    correlation: Option<&str>,
    worker_pid: i32,
) -> Result<()> {
    let conn = crate::db::open(root).context("opening the ledger to record a spawn edge")?;
    crate::db::init_schema(&conn)?;
    conn.execute(
        "INSERT INTO code_spawn_edges (worker_session, spawner, correlation, worker_pid)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(worker_session) DO UPDATE SET
           spawner = excluded.spawner,
           correlation = excluded.correlation,
           worker_pid = excluded.worker_pid,
           settled_at = NULL",
        rusqlite::params![worker_session, spawner, correlation, worker_pid],
    )?;
    Ok(())
}

/// Atomically claim a spawn edge's settle (the worker's own completion path).
/// Opens its own connection (the worker wrapper has no daemon connection). Returns
/// `NoEdge` when no edge is recorded (the caller mails unconditionally).
pub fn claim_spawn_edge(root: &Root, worker_session: &str) -> SettleClaim {
    let Ok(conn) = crate::db::open(root) else {
        return SettleClaim::NoEdge;
    };
    if crate::db::init_schema(&conn).is_err() {
        return SettleClaim::NoEdge;
    }
    claim_spawn_edge_on(&conn, worker_session).unwrap_or(SettleClaim::NoEdge)
}

/// The connection-level claim, shared by the worker path and the daemon reaper.
pub fn claim_spawn_edge_on(
    conn: &rusqlite::Connection,
    worker_session: &str,
) -> Result<SettleClaim> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM code_spawn_edges WHERE worker_session = ?1",
            [worker_session],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !exists {
        return Ok(SettleClaim::NoEdge);
    }
    let n = conn.execute(
        "UPDATE code_spawn_edges
            SET settled_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE worker_session = ?1 AND settled_at IS NULL",
        [worker_session],
    )?;
    Ok(if n == 1 {
        SettleClaim::Claimed
    } else {
        SettleClaim::AlreadySettled
    })
}

/// Release a claimed settle back to unclaimed — used when the claimer WON the
/// claim but then failed to mail the completion, so the fallback producer (the
/// reaper, next tick) can retry rather than the completion being lost.
pub fn unclaim_spawn_edge(root: &Root, worker_session: &str) {
    if let Ok(conn) = crate::db::open(root) {
        let _ = conn.execute(
            "UPDATE code_spawn_edges SET settled_at = NULL WHERE worker_session = ?1",
            [worker_session],
        );
    }
}

/// The unsettled spawn edges whose wrapper pid is dead — reap candidates for the
/// daemon sweep. Liveness uses the same signal-0 probe as every other reaper
/// (`pid_alive`, EPERM-as-alive), so a live cross-uid worker is never wrongly
/// reaped. The caller must still CLAIM each (via `claim_spawn_edge_on`) before
/// mailing, so it races safely with a worker finishing in the same tick.
pub fn dead_unsettled_spawn_edges(conn: &rusqlite::Connection) -> Vec<SpawnEdge> {
    let mut out = Vec::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT worker_session, spawner, correlation, worker_pid
           FROM code_spawn_edges WHERE settled_at IS NULL",
    ) else {
        return out;
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok(SpawnEdge {
            worker_session: r.get(0)?,
            spawner: r.get(1)?,
            correlation: r.get::<_, Option<String>>(2)?,
            worker_pid: r.get(3)?,
        })
    }) else {
        return out;
    };
    for edge in rows.filter_map(|r| r.ok()) {
        if !pid_alive(edge.worker_pid) {
            out.push(edge);
        }
    }
    out
}

/// The session-id prefix that marks a coding-session actor everywhere (CONNECT
/// resolution, ACL, reaping). A principal name starting with this is resolved
/// through this module, never the full-authority fenced-secret path.
pub const PREFIX: &str = "code-";

/// The CLASS of a connected principal — a *label on* the authority the broker
/// already resolved by store placement, never a second source of authority
/// (docs/handoffs/principal-kind.md).
///
/// `elanus` decides *authority* by which fenced store a credential resolves in
/// (session-store → grant-scoped, fenced-secret → full authority, actors-map →
/// grant-scoped package — docs/security.md entry 20). `kind` is set FROM that
/// resolution and carried alongside `(actor, sender)` so downstream readers and
/// humans don't have to re-derive the class from the spelling of a name. It must
/// AGREE with the store the credential resolved in; it never overrides it. Code
/// that decides authority from `kind` alone is the bug this type is designed to
/// avoid.
///
/// Values:
/// - `Session` — a grant-scoped coding worker (`code-*`, session-store token).
/// - `Human`   — a full-authority human identity (the owner or another human;
///               a fenced secret named for the principal).
/// - `Kernel`  — the kernel's own machinery (the fenced secret `kernel`).
/// - `Package` — a supervisor-minted package actor (the in-memory actors map).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PrincipalKind {
    /// A grant-scoped coding worker. The default for a `SessionToken` and for a
    /// `code_sessions` row that predates the stored-`kind` column (they are all
    /// coding runs by construction — the back-compat fallback).
    #[default]
    Session,
    /// A full-authority human identity (owner or another human).
    Human,
    /// The kernel's own machinery.
    Kernel,
    /// A supervisor-minted package actor, grant-scoped to its grants.
    Package,
}

impl PrincipalKind {
    /// Parse a stored `kind` string (token JSON or `code_sessions.kind`).
    /// Unknown/garbage values yield `None` so the caller falls back to the
    /// prefix test rather than silently misclassifying.
    pub fn from_stored(s: &str) -> Option<Self> {
        match s {
            "session" => Some(Self::Session),
            "human" => Some(Self::Human),
            "kernel" => Some(Self::Kernel),
            "package" => Some(Self::Package),
            _ => None,
        }
    }

    /// The canonical stored spelling (matches the serde `rename_all` lowercase).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Human => "human",
            Self::Kernel => "kernel",
            Self::Package => "package",
        }
    }
}

/// Every harness-controlled authority dimension for a session principal, unified
/// into one value. Carried on `SessionToken` via `#[serde(flatten)]` so the
/// on-disk JSON shape is UNCHANGED — tokens written by M1 (flat
/// `publish`/`subscribe`/`turn_budget`/`remaining_budget` fields) still
/// deserialize byte-for-byte (docs/handoffs/authority-delegation.md M2/M3).
///
/// ## Dimensions
///
/// - **Bus capability (non-fungible):** `publish` and `subscribe` are MQTT-filter
///   vecs — the broker gates every publish/subscribe against these.
///   `child ⊆ spawner` (every child filter must be `covers`-ed by some spawner
///   filter) is asserted at mint (docs/security.md entry 22).
/// - **Budget (fungible):** `turn_budget` / `remaining_budget` — the M1 dimension;
///   `Σ children ≤ parent.remaining` is asserted at mint.
/// - **fs_write (non-fungible, M3):** absolute path prefixes the child may
///   request write leases on. `None` = unbounded (the owner-spawned / common
///   case — today's sessions). `Some(set)` = exactly this set of path prefixes.
///   `child ⊆ spawner` checked at mint via `topic::path_covered` (component-wise
///   prefix containment). Runtime enforcement stays as-is: the cage (exec.rs
///   `acquire_lease`) does real canonicalization + `starts_with` at lease time,
///   driven by the profile `[sandbox] fs_write` — M3 adds the mint-time contract
///   ON TOP; it does NOT change the cage.
/// - **fs_read (non-fungible, M3):** absolute path prefixes the child may read
///   from. `None` = unbounded. `Some(set)` = exactly this set.
///   MINT-BOUND ONLY in M3: the containment is recorded and subset-checked at
///   spawn; runtime enforcement is deferred (docs/sandbox.md defers read-scoping,
///   same rationale). `child ⊆ spawner` via `topic::path_covered`.
/// - **tool_allowlist (non-fungible, M3):** exact tool/command names the child
///   may invoke. `None` = unbounded. `Some(set)` = exactly this set.
///   MINT-BOUND ONLY in M3: recorded + subset-checked at spawn; runtime
///   enforcement (blocking the tool call) is deferred. `child ⊆ spawner` via
///   exact-string membership.
/// - **blocking (non-fungible, M3):** exact blocking-class names (e.g. package
///   hook-points) the child may use. `None` = unbounded. `Some(set)` = exactly
///   this set. MINT-BOUND ONLY in M3 (same rationale as tool_allowlist).
///   `child ⊆ spawner` via exact-string membership.
///
/// For fs_read / tool_allowlist / blocking: `None` on old tokens deserializes
/// correctly (back-compat invariant — `#[serde(default)]` produces `None`).
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
    /// Absolute path prefixes the session may request write leases on. `None` =
    /// unbounded (owner / common case). Checked at mint (child ⊆ spawner via
    /// `topic::path_covered`); cage enforces at runtime independently (M3).
    /// Absent in pre-M3 tokens → `None` (back-compat).
    #[serde(default)]
    pub fs_write: Option<Vec<String>>,
    /// Absolute path prefixes the session may read from. `None` = unbounded.
    /// MINT-BOUND ONLY in M3 (runtime enforcement deferred — docs/sandbox.md).
    /// Absent in pre-M3 tokens → `None` (back-compat).
    #[serde(default)]
    pub fs_read: Option<Vec<String>>,
    /// Exact tool/command names this session may invoke. `None` = unbounded.
    /// MINT-BOUND ONLY in M3 (runtime enforcement deferred).
    /// Absent in pre-M3 tokens → `None` (back-compat).
    #[serde(default)]
    pub tool_allowlist: Option<Vec<String>>,
    /// Exact blocking-class names this session may use. `None` = unbounded.
    /// MINT-BOUND ONLY in M3 (runtime enforcement deferred).
    /// Absent in pre-M3 tokens → `None` (back-compat).
    #[serde(default)]
    pub blocking: Option<Vec<String>>,
}

/// The request side of a mint: what the caller asks for on behalf of the child.
///
/// All fields are `Option`; `None` on every field means "inherit-equal from the
/// spawner" — the same default that M1/M2 used for budget and bus scope. Pass
/// `RequestedGrants::default()` at every call site that wants the existing
/// inherit-equal behavior with zero behavior change.
///
/// Used by `mint` to replace the previous 4-parameter `requested_*` argument list
/// (removing the `#[allow(clippy::too_many_arguments)]` + TODO M4 comment).
#[derive(Debug, Clone, Default)]
pub struct RequestedGrants {
    /// Requested turn budget for the child. `None` = inherit-equal (see M1).
    pub budget: Option<u64>,
    /// Requested publish filters. `None` = use structural default (own obs subtree).
    pub publish: Option<Vec<String>>,
    /// Requested subscribe filters. `None` = empty (no read authority today).
    pub subscribe: Option<Vec<String>>,
    /// Requested fs_write path prefixes. `None` = inherit-equal from spawner.
    pub fs_write: Option<Vec<String>>,
    /// Requested fs_read path prefixes. `None` = inherit-equal from spawner.
    pub fs_read: Option<Vec<String>>,
    /// Requested tool_allowlist entries. `None` = inherit-equal from spawner.
    pub tool_allowlist: Option<Vec<String>>,
    /// Requested blocking entries. `None` = inherit-equal from spawner.
    pub blocking: Option<Vec<String>>,
}

/// Resolved M3 capability dimensions for a child session: (fs_write, fs_read,
/// tool_allowlist, blocking). Each is `None` (unbounded) or `Some(narrowed_set)`.
/// Produced by `Grants::narrow_m3_dims` and consumed by `mint`.
type M3Dims = (
    Option<Vec<String>>,
    Option<Vec<String>>,
    Option<Vec<String>>,
    Option<Vec<String>>,
);

impl Grants {
    /// Compute the child's grants from the spawner's grants and the child's
    /// request, asserting `child ⊆ spawner` for every capability dimension and
    /// returning the resolved child `Grants` (or a clear, dimension-named,
    /// entry-22-citing error on any widening).
    ///
    /// ## Rule per dimension (docs/handoffs/authority-delegation.md M3)
    ///
    /// - If `spawner.dim` is `None` (unbounded): child gets `request.dim` (or
    ///   `None`, i.e. also unbounded — inherit-equal).
    /// - If `spawner.dim` is `Some(set)`: child defaults to `Some(set)` (inherit-
    ///   equal) unless the request narrows it; every child entry must be covered
    ///   (path_covered for fs_write/fs_read; exact-string membership for
    ///   tool_allowlist/blocking) by the spawner set, else error.
    ///
    /// Budget keeps its M1 Σ≤ rule (handled separately in `mint` because it
    /// requires a write-back to the spawner token). Bus publish/subscribe keep
    /// their M2 `covers()` rule (also handled in `mint`). This helper resolves
    /// the M3 dimensions only — the caller passes the resolved M1/M2 values in.
    ///
    /// ## Conservative deny
    ///
    /// "When in doubt, deny" — the same doctrine as `topic::covers` and
    /// `topic::path_covered`. An unrecognized entry, empty-set widening, or
    /// any other ambiguity → bail with a dimension-named error.
    ///
    /// ## Cite
    ///
    /// docs/security.md entry 22 [M3] — every authority dimension a spawn
    /// confers is `⊆` the spawner's, by one decidable check per dimension.
    pub fn narrow_m3_dims(
        spawner: &Grants,
        request: &RequestedGrants,
        spawner_name: &str,
    ) -> Result<M3Dims> {
        let fs_write = Self::narrow_path_dim(
            &spawner.fs_write,
            &request.fs_write,
            "fs_write",
            spawner_name,
        )?;
        let fs_read =
            Self::narrow_path_dim(&spawner.fs_read, &request.fs_read, "fs_read", spawner_name)?;
        let tool_allowlist = Self::narrow_set_dim(
            &spawner.tool_allowlist,
            &request.tool_allowlist,
            "tool_allowlist",
            spawner_name,
        )?;
        let blocking = Self::narrow_set_dim(
            &spawner.blocking,
            &request.blocking,
            "blocking",
            spawner_name,
        )?;
        Ok((fs_write, fs_read, tool_allowlist, blocking))
    }

    /// Resolve one path-based capability dimension (fs_write or fs_read).
    ///
    /// - spawner `None` (unbounded): child = request (or None → unbounded).
    /// - spawner `Some(wide)`: child defaults to `Some(wide)` unless request
    ///   narrows it; every child entry must be `path_covered` by the wide set.
    fn narrow_path_dim(
        spawner_dim: &Option<Vec<String>>,
        request_dim: &Option<Vec<String>>,
        dim_name: &str,
        spawner_name: &str,
    ) -> Result<Option<Vec<String>>> {
        match spawner_dim {
            None => {
                // Spawner unbounded: child may request any value (or None).
                Ok(request_dim.clone())
            }
            Some(wide) => {
                // Spawner bounded: child inherits the spawner's set unless it
                // explicitly requests a narrower one.
                let child_entries = request_dim.as_deref().unwrap_or(wide.as_slice());
                for entry in child_entries {
                    if !crate::topic::path_covered(wide, entry) {
                        bail!(
                            "{dim_name} refused (docs/security.md entry 22): child entry \
                             {entry:?} is not within spawner {spawner_name:?}'s {dim_name} \
                             prefixes {wide:?} — child ⊆ spawner violated"
                        );
                    }
                }
                Ok(Some(child_entries.to_vec()))
            }
        }
    }

    /// Resolve one set-membership capability dimension (tool_allowlist or blocking).
    ///
    /// - spawner `None` (unbounded): child = request (or None → unbounded).
    /// - spawner `Some(set)`: child defaults to `Some(set)` unless request
    ///   narrows it; every child entry must be an exact member of the spawner set.
    fn narrow_set_dim(
        spawner_dim: &Option<Vec<String>>,
        request_dim: &Option<Vec<String>>,
        dim_name: &str,
        spawner_name: &str,
    ) -> Result<Option<Vec<String>>> {
        match spawner_dim {
            None => {
                // Spawner unbounded: child may request any value (or None).
                Ok(request_dim.clone())
            }
            Some(allowed) => {
                // Spawner bounded: child inherits the spawner's set unless it
                // explicitly requests a narrower one.
                let child_entries = request_dim.as_deref().unwrap_or(allowed.as_slice());
                for entry in child_entries {
                    if !allowed.contains(entry) {
                        bail!(
                            "{dim_name} refused (docs/security.md entry 22): child entry \
                             {entry:?} is not in spawner {spawner_name:?}'s {dim_name} \
                             set {allowed:?} — child ⊆ spawner violated"
                        );
                    }
                }
                Ok(Some(child_entries.to_vec()))
            }
        }
    }
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
    /// The secret the child presents as LANIUS_BUS_TOKEN.
    pub secret: String,
    /// The launcher pid that owns this session — used by the reaper to tell a
    /// live session's token from an orphan a SIGKILL left behind.
    pub owner_pid: i32,
    /// The class of this principal — always `Session` for a session token (it is
    /// a grant-scoped coding worker by construction). A *label* on the authority
    /// the broker resolves by store placement, not a source of authority
    /// (docs/handoffs/principal-kind.md). `#[serde(default)]` so every token
    /// written before this field existed deserializes byte-for-byte and reports
    /// `kind == Session` (the same back-compat discipline `Grants` uses at the
    /// `#[serde(default)]` fields above).
    #[serde(default)]
    pub kind: PrincipalKind,
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

/// The stored class of a session, read from the durable `code_sessions` record
/// (`kind` column, principal-kind handoff M1). `None` when the session has no
/// row, or the row predates the `kind` column, or the stored value is garbage —
/// in every such case the caller falls back to the name-prefix test.
///
/// This is a pure read over the ledger DB; it decides no authority.
pub fn stored_session_kind(conn: &rusqlite::Connection, session: &str) -> Option<PrincipalKind> {
    conn.query_row(
        "SELECT kind FROM code_sessions WHERE elanus_session = ?1",
        [session],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .ok()
    .flatten() // Result → Option (query ok) → the row's Option<String>
    .flatten() // NULL kind → None
    .as_deref()
    .and_then(PrincipalKind::from_stored)
}

/// Is this session a coding WORKER, for the UI projections that must hide coding
/// runs from the human comms/mail views (web comms-list eviction `web.rs`,
/// mailcli session-mail filter)? This is the SINGLE definition shared by both,
/// replacing the two independent `starts_with("code-")` copies
/// (docs/handoffs/principal-kind.md M3).
///
/// Decides from the durable stored `kind` (`kind == Session`) when the
/// `code_sessions` row carries it; falls back to the name-prefix test
/// (`is_session_principal`) for a row that is absent or predates the column.
/// The decision now comes from what the kernel RECORDED about the principal, not
/// from how its id happens to be spelled — a forward-looking non-`code-` id with
/// `kind = session` is correctly classified as a worker, and a legacy `code-` id
/// with no stored kind is still classified via the fallback.
///
/// This is a UI classifier only; it grants and gates nothing.
pub fn is_worker_session(conn: &rusqlite::Connection, session: &str) -> bool {
    match stored_session_kind(conn, session) {
        Some(kind) => kind == PrincipalKind::Session,
        None => is_session_principal(session),
    }
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

fn structural_publish_filter(agent: &str, principal: &str) -> String {
    // Structural scope: exactly the session's own obs subtree, encoded the same
    // way codeagent::obs_topic encodes the agent/session segments so the filter
    // and the published topics agree even for names with reserved characters.
    format!(
        "obs/agent/{}/{}/#",
        crate::topic::encode_segment(agent),
        crate::topic::encode_segment(principal),
    )
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
/// the token (the launcher hands `.secret` to the child as LANIUS_BUS_TOKEN).
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
///
/// ## M3 capability dimensions (docs/handoffs/authority-delegation.md M3)
///
/// `requested.fs_write`, `requested.fs_read`, `requested.tool_allowlist`, and
/// `requested.blocking` fold the remaining non-fungible dimensions into the same
/// `⊆` contract. `None` on any field = inherit-equal (common case, zero behavior
/// change for all existing call sites that pass `RequestedGrants::default()`).
///
/// - **fs_write:** checked via `topic::path_covered` (component-wise prefix). The
///   cage (exec.rs `acquire_lease`) enforces at runtime independently — M3 adds
///   the mint-time contract only; the runtime cage is unchanged.
/// - **fs_read / tool_allowlist / blocking:** MINT-BOUND ONLY in M3 (recorded +
///   subset-checked at spawn; runtime enforcement deferred, same as
///   docs/sandbox.md defers read-scoping). `fs_read` via `path_covered`;
///   tool_allowlist / blocking via exact-string set membership.
pub fn mint(
    root: &Root,
    principal: &str,
    agent: &str,
    owner_pid: i32,
    spawner: Option<&str>,
    requested: RequestedGrants,
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

    let own_obs = structural_publish_filter(agent, principal);

    // ── Budget + bus-scope + M3 capability invariants: serialized by same lock ──
    //
    // When the spawner token file EXISTS on disk, all reads/checks/writes for
    // ALL dimensions — budget, bus-scope (M2), and fs_write/fs_read/tool/blocking
    // (M3) — are serialized under the same exclusive flock(LOCK_EX). This is the
    // same discipline M1 established; M2/M3 piggyback on it.
    //
    // Lock-free fast paths (zero behavior change for the common case):
    //   - spawner=None (owner/top-of-chain): no spawner file to check.
    //   - spawner file genuinely ABSENT (pre-M1 token, or owner context):
    //     unbounded on all dimensions; no lock needed.
    let (
        child_budget,
        child_publish,
        child_subscribe,
        child_fs_write,
        child_fs_read,
        child_tool_allowlist,
        child_blocking,
    ) = if let Some(spawner_name) = spawner {
        let spawner_token_path = token_path(root, spawner_name);
        // Use the file's EXISTENCE as the branch signal (M1's fail-closed
        // discipline: see the M1 comment above for the full rationale).
        let file_exists = spawner_token_path.try_exists().unwrap_or(true);
        if !file_exists {
            // Spawner token file is genuinely absent (owner context or pre-M1
            // session): treat as unbounded on all dimensions.
            let pub_vec = requested.publish.unwrap_or_else(|| vec![own_obs.clone()]);
            let sub_vec = requested.subscribe.unwrap_or_default();
            (
                requested.budget,
                pub_vec,
                sub_vec,
                requested.fs_write,
                requested.fs_read,
                requested.tool_allowlist,
                requested.blocking,
            )
        } else {
            // Spawner token file EXISTS → acquire the lock before reading,
            // checking, and writing back. Covers all dimensions.
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

            // ── Bus-scope: child ⊆ spawner (M2) ────────────────────────────
            //
            // The child's structural own-obs subtree is ALWAYS allowed — it is
            // the session's own audit trail and NOT a widening of the spawner's
            // authority. Every OTHER publish filter the child requests must be
            // covered by some spawner publish filter.
            //
            // subscribe: every child filter must be covered by some spawner
            // subscribe filter. Today spawner.subscribe is empty and the default
            // child.subscribe is empty (trivially satisfies ⊆).
            let child_pub_req = requested.publish.unwrap_or_else(|| vec![own_obs.clone()]);
            let child_sub_req = requested.subscribe.unwrap_or_default();

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

            // ── M3 capability dimensions: child ⊆ spawner ──────────────────
            //
            // fs_write / fs_read: component-wise path-prefix containment.
            // tool_allowlist / blocking: exact-string set membership.
            // All checked via Grants::narrow_m3_dims (inside the same lock so
            // no concurrent sibling can see a stale spawner token).
            let (child_fw, child_fr, child_ta, child_bl) = {
                let req = RequestedGrants {
                    budget: requested.budget,
                    publish: None, // already resolved above
                    subscribe: None,
                    fs_write: requested.fs_write,
                    fs_read: requested.fs_read,
                    tool_allowlist: requested.tool_allowlist,
                    blocking: requested.blocking,
                };
                Grants::narrow_m3_dims(&spawner_tok.grants, &req, spawner_name)?
            };

            // ── Budget: Σ children ≤ parent.remaining ──────────────────────
            let child_budget = match spawner_tok.grants.remaining_budget {
                None => {
                    // Token present and parseable but no budget cap (pre-M1 or
                    // owner-path token): treat as unbounded — no decrement.
                    requested.budget
                }
                Some(parent_remaining) => {
                    let child_alloc = requested.budget.unwrap_or(parent_remaining);
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

            (
                child_budget,
                child_pub_req,
                child_sub_req,
                child_fw,
                child_fr,
                child_ta,
                child_bl,
            )
        }
    } else {
        // Owner (no spawner session): no dimension checks needed. The child
        // gets its requested/default scope unconditionally — zero behavior
        // change for the owner-spawned common case.
        let pub_vec = requested.publish.unwrap_or_else(|| vec![own_obs.clone()]);
        let sub_vec = requested.subscribe.unwrap_or_default();
        (
            requested.budget,
            pub_vec,
            sub_vec,
            requested.fs_write,
            requested.fs_read,
            requested.tool_allowlist,
            requested.blocking,
        )
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
        // A minted session token is always a grant-scoped coding worker. Stamped
        // explicitly (going forward) so the field is present on disk; the
        // `#[serde(default)]` keeps pre-migration tokens reading as `Session`.
        kind: PrincipalKind::Session,
        grants: Grants {
            publish: child_publish,
            subscribe: child_subscribe,
            turn_budget: child_budget,
            remaining_budget: child_budget,
            fs_write: child_fs_write,
            fs_read: child_fs_read,
            tool_allowlist: child_tool_allowlist,
            blocking: child_blocking,
        },
    };
    write_0600(
        &token_path(root, principal),
        &serde_json::to_string(&token)?,
    )?;
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
/// lanius liveness sweep (release_dead_leases, orphaned-dispatch cleanup).
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

/// Public liveness probe (same signal-0 semantics as `pid_alive`) for the
/// human-facing rooms projection, which renders a member's liveness honestly.
pub fn pid_alive_pub(pid: i32) -> bool {
    pid_alive(pid)
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
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "write_0600: path has no parent",
        )
    })?;
    let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        "{}.tmp.{}.{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("token"),
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
            "lanius-codesess-{}-{}",
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

    // ── principal-kind handoff M1: `kind` is a stored field, back-compat ──────

    /// A freshly minted token carries `"kind":"session"` on disk, and a token
    /// written BEFORE this field existed (no `kind` key) still deserializes
    /// byte-for-byte and resolves as `PrincipalKind::Session` (the `#[serde(default)]`
    /// invariant — same discipline `Grants` uses for its optional fields).
    #[test]
    fn legacy_token_has_no_kind_key_resolves_as_session() {
        let root = tmp_root();

        // Fresh mint stamps the field explicitly.
        let minted = mint(
            &root,
            "code-feedface",
            "claude-code",
            42,
            None,
            RequestedGrants::default(),
        )
        .unwrap();
        assert_eq!(minted.kind, PrincipalKind::Session);
        let on_disk = std::fs::read_to_string(token_path(&root, "code-feedface")).unwrap();
        assert!(
            on_disk.contains("\"kind\":\"session\""),
            "freshly minted token must serialize its kind: {on_disk}"
        );

        // A PRE-MIGRATION token: the exact flat JSON shape a prior version wrote,
        // with NO `kind` key. It must round-trip through `read` unchanged and
        // report kind == Session via the serde default.
        let legacy = r#"{"principal":"code-0badf00d","agent":"codex","secret":"s3cr3t","owner_pid":7}"#;
        std::fs::create_dir_all(store_dir(&root)).unwrap();
        std::fs::write(token_path(&root, "code-0badf00d"), legacy).unwrap();
        let tok = read(&root, "code-0badf00d").expect("legacy token must still deserialize");
        assert_eq!(tok.kind, PrincipalKind::Session);
        assert_eq!(tok.principal, "code-0badf00d");
        assert_eq!(tok.agent, "codex");
        assert_eq!(tok.secret, "s3cr3t");
        assert_eq!(tok.owner_pid, 7);
    }

    /// The `PrincipalKind` stored spelling parses back exactly, and garbage
    /// yields `None` so the caller falls back to the prefix test rather than
    /// misclassifying.
    #[test]
    fn principal_kind_stored_roundtrip() {
        for k in [
            PrincipalKind::Session,
            PrincipalKind::Human,
            PrincipalKind::Kernel,
            PrincipalKind::Package,
        ] {
            assert_eq!(PrincipalKind::from_stored(k.as_str()), Some(k));
        }
        assert_eq!(PrincipalKind::from_stored("bogus"), None);
        assert_eq!(PrincipalKind::from_stored(""), None);
        assert_eq!(PrincipalKind::default(), PrincipalKind::Session);
    }

    // ── principal-kind handoff M3: the shared UI worker classifier ────────────

    /// `is_worker_session` decides from the stored `kind` when the `code_sessions`
    /// row carries it, and falls back to the name prefix otherwise. Proves the
    /// decision now comes from the recorded FIELD, not the spelling:
    ///  - a row with `kind = session` but a NON-`code-` id is a worker (field wins);
    ///  - a legacy `code-` id with no row is still a worker (prefix fallback);
    ///  - a row explicitly marked `human` is NOT a worker even if its id were `code-`;
    ///  - a plain non-`code-` id with no row is not a worker.
    #[test]
    fn is_worker_session_reads_kind_then_prefix() {
        let root = tmp_root();
        let conn = crate::db::open(&root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let insert = |id: &str, kind: &str| {
            conn.execute(
                "INSERT INTO code_sessions
                   (elanus_session, native_session, tool, agent_noun, workdir, kind)
                 VALUES (?1, 'nat', 'claude', 'claude-code', '/tmp', ?2)",
                rusqlite::params![id, kind],
            )
            .unwrap();
        };

        // Forward-looking id shape: NOT `code-`, but recorded as a session.
        insert("sess-abc123", "session");
        assert!(
            is_worker_session(&conn, "sess-abc123"),
            "kind=session must classify as a worker regardless of id spelling"
        );

        // A row explicitly a human is never a worker (even though this id happens
        // to be `code-`-shaped — the field overrides the prefix fallback).
        insert("code-humanish", "human");
        assert!(
            !is_worker_session(&conn, "code-humanish"),
            "kind=human must NOT be classified as a worker"
        );

        // Legacy `code-` id with NO row → prefix fallback classifies it a worker.
        assert!(
            is_worker_session(&conn, "code-deadbeef"),
            "a code-* id with no stored kind falls back to the prefix test"
        );

        // A non-worker, non-code id with no row → not a worker.
        assert!(!is_worker_session(&conn, "kestrel"));
        assert!(!is_worker_session(&conn, "web-1234"));
    }

    #[test]
    fn mint_scope_is_only_the_own_obs_subtree() {
        let root = tmp_root();
        let tok = mint(
            &root,
            "code-deadbeef",
            "claude-code",
            999_999,
            None,
            RequestedGrants::default(),
        )
        .unwrap();
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
        let minted = mint(
            &root,
            "code-cafef00d",
            "claude-code",
            1234,
            None,
            RequestedGrants::default(),
        )
        .unwrap();
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
        mint(
            &root,
            "code-deadbeef",
            "claude-code",
            dead_pid,
            None,
            RequestedGrants::default(),
        )
        .unwrap();
        let live = mint(
            &root,
            "code-livesess",
            "claude-code",
            std::process::id() as i32,
            None,
            RequestedGrants::default(),
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
        // place rather than duplicating — the lanius session is the stable key.
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
        let token = mint(
            &root,
            "code-resume01",
            "codex",
            std::process::id() as i32,
            None,
            RequestedGrants::default(),
        )
        .unwrap();
        assert!(token.may_publish("obs/agent/codex/code-resume01/session/resume"));
        assert!(!token.may_publish("in/human/owner"));
        assert!(
            token.grants.subscribe.is_empty(),
            "resume token must be emit-only"
        );
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
        assert!(
            wrong_noun.is_empty(),
            "a different noun reads its own empty mailbox, not another session's"
        );

        // A non-session name has no inbox at all (no crafted topic).
        assert!(inbox_for_session(&root, "codex", "owner", false)
            .unwrap()
            .is_empty());
        assert!(inbox_for_session(&root, "codex", "code-../escape", false)
            .unwrap()
            .is_empty());
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
        assert_eq!(
            inbox_for_session(&root, "codex", "code-seen0001", true)
                .unwrap()
                .len(),
            2
        );
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
        assert_eq!(
            inbox_for_session(&root, "codex", "code-seen0001", true)
                .unwrap()
                .len(),
            1
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn note_round_trips_and_clears() {
        let root = tmp_root();
        assert!(get_note(&root, "code-note0001").unwrap().is_none());
        set_note(&root, "code-note0001", "  remember the migration  ").unwrap();
        assert_eq!(
            get_note(&root, "code-note0001").unwrap().as_deref(),
            Some("remember the migration")
        );
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
        assert_eq!(
            own_claims(&root, "room-1", "code-aaaa0001").unwrap().len(),
            1
        );

        // unclaim clears only the holder's own claim; idempotent.
        assert!(remove_claim(&root, "room-1", "code-aaaa0001", "src/foo.rs").unwrap());
        assert!(!remove_claim(&root, "room-1", "code-aaaa0001", "src/foo.rs").unwrap());
        assert!(peer_claims(&root, "room-1", "code-bbbb0002")
            .unwrap()
            .is_empty());
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
        assert_eq!(
            peer_claims(&root, "room-r", "code-stays002").unwrap().len(),
            2
        );
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
        assert!(peer_claims(&root, "room-z", "code-liveone2")
            .unwrap()
            .is_empty());
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
        let tok = mint(
            &root,
            "code-budget001",
            "claude-code",
            999_999,
            None,
            RequestedGrants::default(),
        )
        .unwrap();
        assert_eq!(tok.grants.turn_budget, None, "owner path must be unbounded");
        assert_eq!(
            tok.grants.remaining_budget, None,
            "owner path must be unbounded"
        );
        // Roundtrip: the token file deserializes back with None budgets.
        let read_back = read(&root, "code-budget001").unwrap();
        assert_eq!(read_back.grants.turn_budget, None);
        assert_eq!(read_back.grants.remaining_budget, None);
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── M2: bus capability dimension + child⊆parent assert ──────────────────

    #[test]
    fn bus_child_cannot_subscribe_outside_spawner_scope() {
        let root = tmp_root();
        let pid = std::process::id() as i32;
        mint(
            &root,
            "code-busparent01",
            "claude-code",
            pid,
            None,
            RequestedGrants::default(),
        )
        .unwrap();

        let result = mint(
            &root,
            "code-buschild01",
            "claude-code",
            pid,
            Some("code-busparent01"),
            RequestedGrants {
                subscribe: Some(vec!["obs/#".to_string()]),
                ..Default::default()
            },
        );

        assert!(
            result.is_err(),
            "subscribe widening must be refused at mint"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("bus subscribe refused"),
            "error must name the refusal class: {err}"
        );
        assert!(read(&root, "code-buschild01").is_none());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn bus_child_cannot_publish_outside_spawner_scope() {
        let root = tmp_root();
        let pid = std::process::id() as i32;
        mint(
            &root,
            "code-busparent02",
            "claude-code",
            pid,
            None,
            RequestedGrants::default(),
        )
        .unwrap();

        let result = mint(
            &root,
            "code-buschild02",
            "claude-code",
            pid,
            Some("code-busparent02"),
            RequestedGrants {
                publish: Some(vec!["work/#".to_string()]),
                ..Default::default()
            },
        );

        assert!(result.is_err(), "publish widening must be refused at mint");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("bus publish refused"),
            "error must name the refusal class: {err}"
        );
        assert!(read(&root, "code-buschild02").is_none());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn bus_default_is_structural_and_owner_request_passes_vacuously() {
        let root = tmp_root();
        let pid = std::process::id() as i32;
        mint(
            &root,
            "code-busparent03",
            "claude-code",
            pid,
            None,
            RequestedGrants::default(),
        )
        .unwrap();

        let child = mint(
            &root,
            "code-buschild03",
            "codex",
            pid,
            Some("code-busparent03"),
            RequestedGrants::default(),
        )
        .unwrap();
        assert_eq!(
            child.grants.publish,
            vec!["obs/agent/codex/code-buschild03/#".to_string()],
            "None,None must grant exactly the child's structural publish baseline"
        );
        assert!(
            child.grants.subscribe.is_empty(),
            "None,None must preserve the empty structural subscribe baseline"
        );

        let owner_child = mint(
            &root,
            "code-busowner01",
            "codex",
            pid,
            None,
            RequestedGrants {
                publish: Some(vec!["work/#".to_string()]),
                subscribe: Some(vec!["obs/#".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(owner_child.grants.publish.contains(&"work/#".to_string()));
        assert!(owner_child.grants.subscribe.contains(&"obs/#".to_string()));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn bus_corrupt_spawner_token_fails_closed_for_scoped_request() {
        let root = tmp_root();
        let pid = std::process::id() as i32;
        let dir = store_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("code-buscorrupt.json");
        std::fs::write(&path, b"{ THIS IS NOT VALID JSON }").unwrap();

        let result = mint(
            &root,
            "code-buschild04",
            "claude-code",
            pid,
            Some("code-buscorrupt"),
            RequestedGrants {
                publish: Some(vec!["work/#".to_string()]),
                ..Default::default()
            },
        );

        assert!(
            result.is_err(),
            "corrupt spawner token must fail closed for bus grant requests"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("could not be parsed"),
            "error must explain the bus fail-closed refusal: {err}"
        );
        assert!(read(&root, "code-buschild04").is_none());
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
        mint(
            &root,
            "code-parent01",
            "claude-code",
            parent_pid,
            None,
            RequestedGrants {
                budget: Some(100),
                ..Default::default()
            },
        )
        .unwrap();
        // Child inherits full remaining (100) via inherit-equal.
        let child = mint(
            &root,
            "code-child001",
            "claude-code",
            parent_pid,
            Some("code-parent01"),
            RequestedGrants::default(),
        )
        .unwrap();
        assert_eq!(
            child.grants.turn_budget,
            Some(100),
            "inherit-equal: child gets parent's full remaining"
        );
        assert_eq!(child.grants.remaining_budget, Some(100));
        // Parent's remaining is now 0 (the child took the full 100).
        let parent_after = read(&root, "code-parent01").unwrap();
        assert_eq!(
            parent_after.grants.remaining_budget,
            Some(0),
            "parent's remaining is decremented by the child's allocation"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn budget_explicit_narrow_child_gets_requested_amount() {
        // When the child explicitly requests a smaller budget than the parent's
        // remaining, it gets exactly what it asked for — narrowing is the point
        // of the RLM case ("halve it to pass context down").
        let root = tmp_root();
        let pid = std::process::id() as i32;
        mint(
            &root,
            "code-parent02",
            "claude-code",
            pid,
            None,
            RequestedGrants {
                budget: Some(100),
                ..Default::default()
            },
        )
        .unwrap();
        let child = mint(
            &root,
            "code-child002",
            "claude-code",
            pid,
            Some("code-parent02"),
            RequestedGrants {
                budget: Some(40),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            child.grants.turn_budget,
            Some(40),
            "explicit narrowing: child gets requested 40"
        );
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
        mint(
            &root,
            "code-parent03",
            "claude-code",
            pid,
            None,
            RequestedGrants {
                budget: Some(50),
                ..Default::default()
            },
        )
        .unwrap();
        // Child requests 51 — one more than the parent has.
        let result = mint(
            &root,
            "code-child003",
            "claude-code",
            pid,
            Some("code-parent03"),
            RequestedGrants {
                budget: Some(51),
                ..Default::default()
            },
        );
        assert!(result.is_err(), "over-allocation must be refused at mint");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("budget allocation refused"),
            "error must name the refusal class: {err}"
        );
        assert!(
            err.contains("code-parent03"),
            "error must name the spawner: {err}"
        );
        // The refused mint must NOT have written a child token.
        assert!(
            read(&root, "code-child003").is_none(),
            "refused mint must not leave a token file"
        );
        // The parent's remaining is unchanged — the failed mint must not charge it.
        let parent_after = read(&root, "code-parent03").unwrap();
        assert_eq!(
            parent_after.grants.remaining_budget,
            Some(50),
            "failed mint must not decrement the spawner's remaining"
        );
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
        mint(
            &root,
            "code-parent04",
            "claude-code",
            pid,
            None,
            RequestedGrants {
                budget: Some(60),
                ..Default::default()
            },
        )
        .unwrap();

        // First child claims 40: succeeds; parent now has 20 remaining.
        let c1 = mint(
            &root,
            "code-sib001",
            "claude-code",
            pid,
            Some("code-parent04"),
            RequestedGrants {
                budget: Some(40),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(c1.grants.turn_budget, Some(40));
        let after_c1 = read(&root, "code-parent04").unwrap();
        assert_eq!(after_c1.grants.remaining_budget, Some(20));

        // Second child claims 21: would push Σ to 61 > 60 → REFUSED.
        let result = mint(
            &root,
            "code-sib002",
            "claude-code",
            pid,
            Some("code-parent04"),
            RequestedGrants {
                budget: Some(21),
                ..Default::default()
            },
        );
        assert!(
            result.is_err(),
            "second sibling must be refused when Σ > parent"
        );
        assert!(
            read(&root, "code-sib002").is_none(),
            "refused sibling must not leave a token"
        );

        // Parent's remaining is still 20 (the failed mint did not charge it).
        let after_fail = read(&root, "code-parent04").unwrap();
        assert_eq!(after_fail.grants.remaining_budget, Some(20));

        // Third sibling that fits (20) succeeds — the partition has exactly 0 left.
        let c3 = mint(
            &root,
            "code-sib003",
            "claude-code",
            pid,
            Some("code-parent04"),
            RequestedGrants {
                budget: Some(20),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(c3.grants.turn_budget, Some(20));
        let after_c3 = read(&root, "code-parent04").unwrap();
        assert_eq!(
            after_c3.grants.remaining_budget,
            Some(0),
            "siblings exactly exhaust the parent's budget — Σ = parent"
        );
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
        assert_eq!(
            tok.grants.turn_budget, None,
            "missing turn_budget in old token → None (unbounded)"
        );
        assert_eq!(
            tok.grants.remaining_budget, None,
            "missing remaining_budget in old token → None (unbounded)"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── M2: bus-scope child ⊆ spawner (docs/handoffs/authority-delegation.md) ───

    /// Helper: mint a spawner, then overwrite its grants to a specific bus scope.
    fn spawner_with_scope(root: &Root, name: &str, publish: &[&str], subscribe: &[&str]) {
        let mut sp = mint(
            root,
            name,
            "claude-code",
            999_999,
            None,
            RequestedGrants::default(),
        )
        .unwrap();
        sp.grants.publish = publish.iter().map(|s| s.to_string()).collect();
        sp.grants.subscribe = subscribe.iter().map(|s| s.to_string()).collect();
        write_0600(
            &token_path(root, name),
            &serde_json::to_string(&sp).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn m2_owner_spawned_default_scope_is_unchanged() {
        // The common case (no spawner) is byte-identical to entry-20/M1: publish
        // exactly the own obs subtree, subscribe nothing.
        let root = tmp_root();
        let tok = mint(
            &root,
            "code-ownerdef",
            "claude-code",
            1,
            None,
            RequestedGrants::default(),
        )
        .unwrap();
        assert_eq!(
            tok.grants.publish,
            vec!["obs/agent/claude-code/code-ownerdef/#".to_string()]
        );
        assert!(tok.grants.subscribe.is_empty());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m2_child_publish_must_be_subset_of_spawner() {
        let root = tmp_root();
        spawner_with_scope(&root, "code-scopep1", &["obs/agent/claude-code/#"], &[]);
        // A child requesting a WIDER publish (obs/#) than the spawner is refused.
        let widen = mint(
            &root,
            "code-cwide1",
            "claude-code",
            1,
            Some("code-scopep1"),
            RequestedGrants {
                publish: Some(vec!["obs/#".to_string()]),
                ..Default::default()
            },
        );
        assert!(widen.is_err(), "widening publish must be refused");
        let err = widen.unwrap_err().to_string();
        assert!(
            err.contains("bus publish refused"),
            "must name the refusal: {err}"
        );
        // A child requesting another agent's subtree (not its own, not under
        // spawner's claude-code-only grant) is refused.
        let cross = mint(
            &root,
            "code-ccross1",
            "claude-code",
            1,
            Some("code-scopep1"),
            RequestedGrants {
                publish: Some(vec!["obs/agent/codex/code-ccross1/#".to_string()]),
                ..Default::default()
            },
        );
        assert!(cross.is_err(), "cross-agent publish must be refused");
        // A child requesting a filter under the spawner's grant succeeds.
        let ok = mint(
            &root,
            "code-cok1",
            "claude-code",
            1,
            Some("code-scopep1"),
            RequestedGrants {
                publish: Some(vec!["obs/agent/claude-code/code-cok1/#".to_string()]),
                ..Default::default()
            },
        );
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
        let tok = mint(
            &root,
            "code-cown2",
            "claude-code",
            1,
            Some("code-scopep2"),
            RequestedGrants {
                publish: Some(vec![own.clone()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(tok.grants.publish, vec![own]);
        // But the child CANNOT borrow the spawner's disjoint subtree for itself
        // unless it explicitly requests it AND it is covered — requesting codex's
        // subtree IS covered by the spawner here, so that is legitimately allowed
        // (it is ⊆ spawner). Requesting something neither own nor ⊆ spawner fails.
        let bad = mint(
            &root,
            "code-cown2b",
            "claude-code",
            1,
            Some("code-scopep2"),
            RequestedGrants {
                publish: Some(vec!["in/human/owner".to_string()]),
                ..Default::default()
            },
        );
        assert!(
            bad.is_err(),
            "a filter neither own nor ⊆ spawner must be refused"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m2_default_child_publish_passes_against_narrow_spawner() {
        // With no requested_publish, the child's default is its own obs subtree —
        // which is always allowed — so a narrow spawner never blocks the default.
        let root = tmp_root();
        spawner_with_scope(&root, "code-scopep3", &["obs/agent/codex/#"], &[]);
        let tok = mint(
            &root,
            "code-cdef3",
            "claude-code",
            1,
            Some("code-scopep3"),
            RequestedGrants::default(),
        )
        .unwrap();
        assert_eq!(
            tok.grants.publish,
            vec!["obs/agent/claude-code/code-cdef3/#".to_string()]
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m2_child_subscribe_must_be_subset_of_spawner() {
        let root = tmp_root();
        // Spawner has a non-empty subscribe scope.
        spawner_with_scope(
            &root,
            "code-scopes1",
            &["obs/agent/claude-code/#"],
            &["obs/agent/claude-code/#"],
        );
        // A child subscribe not covered by the spawner's is refused.
        let widen = mint(
            &root,
            "code-csub1",
            "claude-code",
            1,
            Some("code-scopes1"),
            RequestedGrants {
                subscribe: Some(vec!["obs/#".to_string()]),
                ..Default::default()
            },
        );
        assert!(widen.is_err(), "widening subscribe must be refused");
        let err = widen.unwrap_err().to_string();
        assert!(
            err.contains("bus subscribe refused"),
            "must name the refusal: {err}"
        );
        // A covered subscribe passes.
        let ok = mint(
            &root,
            "code-csub2",
            "claude-code",
            1,
            Some("code-scopes1"),
            RequestedGrants {
                subscribe: Some(vec!["obs/agent/claude-code/code-x/#".to_string()]),
                ..Default::default()
            },
        );
        assert!(ok.is_ok(), "subscribe ⊆ spawner must pass: {ok:?}");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m2_default_empty_subscribe_passes_under_empty_spawner() {
        // The default child subscribe is empty, which trivially satisfies ⊆ even
        // when the spawner's subscribe is also empty (today's structural default).
        let root = tmp_root();
        spawner_with_scope(&root, "code-scopes2", &["obs/agent/claude-code/#"], &[]);
        let tok = mint(
            &root,
            "code-csub3",
            "claude-code",
            1,
            Some("code-scopes2"),
            RequestedGrants::default(),
        )
        .unwrap();
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
        assert_eq!(
            tok.grants.publish,
            vec!["obs/agent/codex/code-m1tok/#".to_string()]
        );
        assert_eq!(tok.grants.turn_budget, Some(5));
        assert_eq!(tok.grants.remaining_budget, Some(2));
        // And it re-serializes to the same flat shape (no nested "grants" key).
        let back = serde_json::to_string(&tok).unwrap();
        assert!(
            back.contains("\"publish\""),
            "publish stays top-level: {back}"
        );
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
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        use std::sync::{Arc, Mutex};

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

            mint(
                &root,
                &parent_name,
                "claude-code",
                pid,
                None,
                RequestedGrants {
                    budget: Some(parent_budget),
                    ..Default::default()
                },
            )
            .unwrap();

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
                            RequestedGrants {
                                budget: Some(per_child),
                                ..Default::default()
                            },
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
            let final_remaining = parent_final
                .grants
                .remaining_budget
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

    // ── M3: fs_write / fs_read / tool_allowlist / blocking ⊆ spawner ────────────
    //
    // Regression tests for docs/handoffs/authority-delegation.md M3 and
    // docs/security.md entry 22 [M3]. They assert at the MINT LAYER — a child
    // session-spawned by a finite-grants spawner cannot widen any M3 dimension.

    /// Helper: mint a spawner and overwrite its M3 capability grants directly.
    fn spawner_with_m3(
        root: &Root,
        name: &str,
        fs_write: Option<Vec<&str>>,
        fs_read: Option<Vec<&str>>,
        tool_allowlist: Option<Vec<&str>>,
        blocking: Option<Vec<&str>>,
    ) {
        let mut sp = mint(
            root,
            name,
            "claude-code",
            999_999,
            None,
            RequestedGrants::default(),
        )
        .unwrap();
        sp.grants.fs_write = fs_write.map(|v| v.iter().map(|s| s.to_string()).collect());
        sp.grants.fs_read = fs_read.map(|v| v.iter().map(|s| s.to_string()).collect());
        sp.grants.tool_allowlist =
            tool_allowlist.map(|v| v.iter().map(|s| s.to_string()).collect());
        sp.grants.blocking = blocking.map(|v| v.iter().map(|s| s.to_string()).collect());
        write_0600(
            &token_path(root, name),
            &serde_json::to_string(&sp).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn m3_owner_spawned_child_inherits_unbounded_m3_dims() {
        // Owner path (spawner=None): child gets all M3 dims as None (unbounded).
        // Zero behavior change for today's common case.
        let root = tmp_root();
        let tok = mint(
            &root,
            "code-m3owner",
            "claude-code",
            1,
            None,
            RequestedGrants::default(),
        )
        .unwrap();
        assert!(
            tok.grants.fs_write.is_none(),
            "owner-spawned fs_write must be None (unbounded)"
        );
        assert!(
            tok.grants.fs_read.is_none(),
            "owner-spawned fs_read must be None (unbounded)"
        );
        assert!(
            tok.grants.tool_allowlist.is_none(),
            "owner-spawned tool_allowlist must be None"
        );
        assert!(
            tok.grants.blocking.is_none(),
            "owner-spawned blocking must be None"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m3_child_fs_write_must_be_within_spawner_prefixes() {
        // THE CRUX: a child cannot request an fs_write path outside the spawner's.
        let root = tmp_root();
        spawner_with_m3(
            &root,
            "code-fw-spawner",
            Some(vec!["/work/project"]),
            None,
            None,
            None,
        );

        // A child requesting a path inside the spawner's prefix: allowed.
        let ok = mint(
            &root,
            "code-fw-child1",
            "claude-code",
            1,
            Some("code-fw-spawner"),
            RequestedGrants {
                fs_write: Some(vec!["/work/project/src".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            ok.grants.fs_write,
            Some(vec!["/work/project/src".to_string()])
        );

        // A child requesting a path OUTSIDE the spawner's prefix: refused.
        let bad = mint(
            &root,
            "code-fw-child2",
            "claude-code",
            1,
            Some("code-fw-spawner"),
            RequestedGrants {
                fs_write: Some(vec!["/etc/passwd".to_string()]),
                ..Default::default()
            },
        );
        assert!(bad.is_err(), "fs_write outside spawner must be refused");
        let err = bad.unwrap_err().to_string();
        assert!(
            err.contains("fs_write refused"),
            "error must name the dimension: {err}"
        );
        assert!(
            err.contains("entry 22"),
            "error must cite the ledger entry: {err}"
        );
        // No token written for the refused mint.
        assert!(read(&root, "code-fw-child2").is_none());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m3_child_fs_write_component_boundary_not_string_prefix() {
        // The component-boundary trap: /work/proj is NOT within /work/project
        // (string prefix would say yes; Path::starts_with says no).
        let root = tmp_root();
        spawner_with_m3(
            &root,
            "code-fw-boundary",
            Some(vec!["/work/project"]),
            None,
            None,
            None,
        );
        let bad = mint(
            &root,
            "code-fw-bchild",
            "claude-code",
            1,
            Some("code-fw-boundary"),
            RequestedGrants {
                fs_write: Some(vec!["/work/projectX".to_string()]),
                ..Default::default()
            },
        );
        assert!(
            bad.is_err(),
            "/work/projectX is NOT within /work/project — component boundary"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m3_child_fs_read_must_be_within_spawner_prefixes() {
        // fs_read: same path_covered rule as fs_write.
        let root = tmp_root();
        spawner_with_m3(
            &root,
            "code-fr-spawner",
            None,
            Some(vec!["/data"]),
            None,
            None,
        );

        let ok = mint(
            &root,
            "code-fr-child1",
            "claude-code",
            1,
            Some("code-fr-spawner"),
            RequestedGrants {
                fs_read: Some(vec!["/data/reports".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(ok.grants.fs_read, Some(vec!["/data/reports".to_string()]));

        let bad = mint(
            &root,
            "code-fr-child2",
            "claude-code",
            1,
            Some("code-fr-spawner"),
            RequestedGrants {
                fs_read: Some(vec!["/secrets".to_string()]),
                ..Default::default()
            },
        );
        assert!(bad.is_err(), "fs_read outside spawner must be refused");
        let err = bad.unwrap_err().to_string();
        assert!(
            err.contains("fs_read refused"),
            "error must name the dimension: {err}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m3_child_tool_allowlist_must_be_subset_of_spawner() {
        // tool_allowlist: exact-string membership.
        let root = tmp_root();
        spawner_with_m3(
            &root,
            "code-ta-spawner",
            None,
            None,
            Some(vec!["Bash", "Read"]),
            None,
        );

        // A child requesting a strict subset: allowed.
        let ok = mint(
            &root,
            "code-ta-child1",
            "claude-code",
            1,
            Some("code-ta-spawner"),
            RequestedGrants {
                tool_allowlist: Some(vec!["Bash".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(ok.grants.tool_allowlist, Some(vec!["Bash".to_string()]));

        // A child requesting a tool NOT in the spawner's allowlist: refused.
        let bad = mint(
            &root,
            "code-ta-child2",
            "claude-code",
            1,
            Some("code-ta-spawner"),
            RequestedGrants {
                tool_allowlist: Some(vec!["Bash".to_string(), "Write".to_string()]),
                ..Default::default()
            },
        );
        assert!(
            bad.is_err(),
            "tool not in spawner allowlist must be refused"
        );
        let err = bad.unwrap_err().to_string();
        assert!(
            err.contains("tool_allowlist refused"),
            "error must name the dimension: {err}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m3_child_blocking_must_be_subset_of_spawner() {
        // blocking: exact-string membership.
        let root = tmp_root();
        spawner_with_m3(
            &root,
            "code-bl-spawner",
            None,
            None,
            None,
            Some(vec!["hook-a"]),
        );

        let ok = mint(
            &root,
            "code-bl-child1",
            "claude-code",
            1,
            Some("code-bl-spawner"),
            RequestedGrants {
                blocking: Some(vec!["hook-a".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(ok.grants.blocking, Some(vec!["hook-a".to_string()]));

        let bad = mint(
            &root,
            "code-bl-child2",
            "claude-code",
            1,
            Some("code-bl-spawner"),
            RequestedGrants {
                blocking: Some(vec!["hook-b".to_string()]),
                ..Default::default()
            },
        );
        assert!(
            bad.is_err(),
            "blocking entry not in spawner set must be refused"
        );
        let err = bad.unwrap_err().to_string();
        assert!(
            err.contains("blocking refused"),
            "error must name the dimension: {err}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m3_inherit_equal_when_no_request() {
        // When the child makes no request (all M3 dims None in RequestedGrants),
        // it inherits the spawner's set exactly — inherit-equal default.
        let root = tmp_root();
        spawner_with_m3(
            &root,
            "code-m3-inh-sp",
            Some(vec!["/work"]),
            Some(vec!["/data"]),
            Some(vec!["Bash"]),
            Some(vec!["hook-x"]),
        );

        let tok = mint(
            &root,
            "code-m3-inh-ch",
            "claude-code",
            1,
            Some("code-m3-inh-sp"),
            RequestedGrants::default(),
        )
        .unwrap();

        assert_eq!(
            tok.grants.fs_write,
            Some(vec!["/work".to_string()]),
            "fs_write: inherit-equal from spawner"
        );
        assert_eq!(
            tok.grants.fs_read,
            Some(vec!["/data".to_string()]),
            "fs_read: inherit-equal from spawner"
        );
        assert_eq!(
            tok.grants.tool_allowlist,
            Some(vec!["Bash".to_string()]),
            "tool_allowlist: inherit-equal from spawner"
        );
        assert_eq!(
            tok.grants.blocking,
            Some(vec!["hook-x".to_string()]),
            "blocking: inherit-equal from spawner"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m3_spawner_unbounded_dim_passes_any_child_value() {
        // When the spawner's M3 dim is None (unbounded), the child may request
        // any value — the owner/unbounded path is unchanged.
        let root = tmp_root();
        // Spawner with all M3 dims unbounded (None).
        spawner_with_m3(&root, "code-m3-unb-sp", None, None, None, None);

        let tok = mint(
            &root,
            "code-m3-unb-ch",
            "claude-code",
            1,
            Some("code-m3-unb-sp"),
            RequestedGrants {
                fs_write: Some(vec!["/arbitrary/path".to_string()]),
                tool_allowlist: Some(vec!["AnyTool".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(
            tok.grants.fs_write,
            Some(vec!["/arbitrary/path".to_string()])
        );
        assert_eq!(tok.grants.tool_allowlist, Some(vec!["AnyTool".to_string()]));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn m3_back_compat_old_token_missing_m3_fields_deserializes_with_none() {
        // Back-compat invariant: a token written BEFORE M3 (no fs_write / fs_read /
        // tool_allowlist / blocking fields) deserializes with all those fields as
        // None (unbounded) — no behavior change for existing sessions.
        let pre_m3 = r#"{"principal":"code-prem3","agent":"codex","secret":"s","owner_pid":3,
                         "publish":["obs/agent/codex/code-prem3/#"],"subscribe":[],
                         "turn_budget":5,"remaining_budget":2}"#;
        let tok: SessionToken = serde_json::from_str(pre_m3).unwrap();
        assert!(
            tok.grants.fs_write.is_none(),
            "pre-M3 token: fs_write must be None"
        );
        assert!(
            tok.grants.fs_read.is_none(),
            "pre-M3 token: fs_read must be None"
        );
        assert!(
            tok.grants.tool_allowlist.is_none(),
            "pre-M3 token: tool_allowlist must be None"
        );
        assert!(
            tok.grants.blocking.is_none(),
            "pre-M3 token: blocking must be None"
        );
        // And it serializes back WITHOUT a nested "grants" key — back-compat shape.
        let back = serde_json::to_string(&tok).unwrap();
        assert!(
            !back.contains("\"grants\""),
            "no nested grants key in output: {back}"
        );
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
            RequestedGrants {
                budget: Some(10),
                ..Default::default()
            },
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

    // ── SA2: live-sibling roster + honest liveness ────────────────────────────

    fn sa2_record(root: &Root, sess: &str, workdir: &str, room: &str, pid: i32) {
        upsert_record(
            root,
            &SessionRecord {
                elanus_session: sess.into(),
                native_session: "t".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: workdir.into(),
                room: Some(room.into()),
            },
        )
        .unwrap();
        join_room(root, room, sess, "codex", pid).unwrap();
    }

    /// Force a session's `last_active` to an old value so the freshness window
    /// ages it out (stale-session hygiene without a real elapsed wait).
    fn backdate(root: &Root, sess: &str) {
        let conn = crate::db::open(root).unwrap();
        conn.execute(
            "UPDATE code_sessions SET last_active = '2000-01-01T00:00:00.000Z' \
             WHERE elanus_session = ?1",
            [sess],
        )
        .unwrap();
    }

    #[test]
    fn live_siblings_lists_a_fresh_same_workdir_peer() {
        let root = tmp_root();
        let wd = root.dir.join("co").display().to_string();
        std::fs::create_dir_all(&wd).unwrap();
        let me = std::process::id() as i32;
        sa2_record(&root, "code-view00001", &wd, "r", me);
        sa2_record(&root, "code-peer00002", &wd, "r", me);
        let sibs = live_siblings(&root, "code-view00001", &wd);
        assert_eq!(sibs.len(), 1, "exactly one live sibling: {sibs:?}");
        assert_eq!(sibs[0].session, "code-peer00002");
        // The viewer is never its own sibling.
        assert!(!sibs.iter().any(|s| s.session == "code-view00001"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn live_siblings_excludes_a_different_workdir() {
        let root = tmp_root();
        let wd_a = root.dir.join("a").display().to_string();
        let wd_b = root.dir.join("b").display().to_string();
        std::fs::create_dir_all(&wd_a).unwrap();
        std::fs::create_dir_all(&wd_b).unwrap();
        let me = std::process::id() as i32;
        sa2_record(&root, "code-aaa00001", &wd_a, "r", me);
        sa2_record(&root, "code-bbb00002", &wd_b, "r", me);
        assert!(
            live_siblings(&root, "code-aaa00001", &wd_a).is_empty(),
            "a peer in a different workdir is not a sibling"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn live_siblings_ages_out_a_stale_session() {
        let root = tmp_root();
        let wd = root.dir.join("co").display().to_string();
        std::fs::create_dir_all(&wd).unwrap();
        let me = std::process::id() as i32;
        sa2_record(&root, "code-view00001", &wd, "r", me);
        sa2_record(&root, "code-stale0002", &wd, "r", me);
        backdate(&root, "code-stale0002"); // last_active far in the past
        let sibs = live_siblings(&root, "code-view00001", &wd);
        assert!(
            sibs.is_empty(),
            "a session past the freshness window must age out: {sibs:?}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn live_siblings_excludes_a_dead_pid() {
        let root = tmp_root();
        let wd = root.dir.join("co").display().to_string();
        std::fs::create_dir_all(&wd).unwrap();
        let me = std::process::id() as i32;
        sa2_record(&root, "code-view00001", &wd, "r", me);
        // Fresh last_active but a pid that is not alive → excluded.
        sa2_record(&root, "code-dead00002", &wd, "r", i32::MAX);
        let sibs = live_siblings(&root, "code-view00001", &wd);
        assert!(
            !sibs.iter().any(|s| s.session == "code-dead00002"),
            "a dead-pid sibling must be excluded: {sibs:?}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── M3: tri-state liveness (agent-situational-awareness handoff) ──────────

    #[test]
    fn classify_liveness_is_tri_state() {
        use DisconnectKind::*;
        use Liveness::*;
        // CONNECTED: broker holds it (or unknown) AND the same-host pid is alive.
        assert_eq!(classify_liveness(Some(true), Some(true)), Connected);
        assert_eq!(classify_liveness(None, Some(true)), Connected);
        // DISCONNECTED (split brain): the broker lost it but the pid is STILL ALIVE
        // — it may be a partitioned agent still editing files. NOT dead.
        assert_eq!(
            classify_liveness(Some(false), Some(true)),
            Disconnected(SplitBrain)
        );
        // DISCONNECTED (unknown): no local pid to probe (cross host) and the broker
        // lost it — death is unconfirmable, so it is never escalated to dead.
        assert_eq!(
            classify_liveness(Some(false), None),
            Disconnected(Unknown)
        );
        // DEAD: a same-host pid probe FAILS — the one signal that confirms death.
        // This holds regardless of the broker's (possibly stale) connection view.
        assert_eq!(classify_liveness(Some(true), Some(false)), Dead);
        assert_eq!(classify_liveness(Some(false), Some(false)), Dead);
        assert_eq!(classify_liveness(None, Some(false)), Dead);
        // No pid but the broker says connected → Connected (positive evidence),
        // never dead (cross-host death is never inferred without a probe).
        assert_eq!(classify_liveness(Some(true), None), Connected);
        // No pid AND no broker view → Disconnected(Unknown), NOT Connected:
        // absence of evidence is not evidence of life. Still never Dead.
        assert_eq!(classify_liveness(None, None), Disconnected(Unknown));
    }

    #[test]
    fn reap_reaps_only_confirmed_dead_never_a_disconnected_split_brain() {
        // THE M3 SAFETY INVARIANT. A DISCONNECTED session whose process is still
        // alive is a possible split brain still editing files — its claims MUST
        // survive the sweep. Only a CONFIRMED-DEAD (pid gone) session is reaped.
        let root = tmp_root();
        let dead_pid = 0x7fff_fffe; // not a live pid on any sane system
        let live_pid = std::process::id() as i32;

        // A DISCONNECTED (broker lost it) session whose pid is ALIVE — a split brain.
        member(&root, "room-sb", "code-splitbrn", live_pid);
        set_connected(&root, "code-splitbrn", false).unwrap();
        add_claim(&root, "room-sb", "code-splitbrn", "still-editing.rs").unwrap();

        // A CONFIRMED-DEAD session (pid gone), also marked disconnected.
        member(&root, "room-sb", "code-deadgone1", dead_pid);
        set_connected(&root, "code-deadgone1", false).unwrap();
        add_claim(&root, "room-sb", "code-deadgone1", "gone.rs").unwrap();

        let reaped = reap_dead_members(&root);

        // The split brain is NOT reaped — its claim stays live for peers to route around.
        assert!(
            !reaped.iter().any(|(_, s)| s == "code-splitbrn"),
            "a disconnected-but-alive split brain must NOT be reaped: {reaped:?}"
        );
        assert_eq!(
            own_claims(&root, "room-sb", "code-splitbrn").unwrap().len(),
            1,
            "the split brain's claim must survive the sweep"
        );
        // The confirmed-dead session IS reaped.
        assert!(
            reaped.contains(&("room-sb".to_string(), "code-deadgone1".to_string())),
            "a confirmed-dead session's claims must be reaped: {reaped:?}"
        );
        assert!(own_claims(&root, "room-sb", "code-deadgone1")
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn live_siblings_keeps_and_flags_a_disconnected_split_brain() {
        // A broker-disconnected sibling with a live pid stays in the roster FLAGGED
        // as a possible split brain — the warning peers need to treat its claims as
        // live (never silently dropped like a merely-aged-out session).
        let root = tmp_root();
        let wd = root.dir.join("co").display().to_string();
        std::fs::create_dir_all(&wd).unwrap();
        let me = std::process::id() as i32;
        sa2_record(&root, "code-view00001", &wd, "r", me);
        sa2_record(&root, "code-splt0002", &wd, "r", me);
        set_connected(&root, "code-splt0002", false).unwrap();
        let sibs = live_siblings(&root, "code-view00001", &wd);
        let sb = sibs.iter().find(|s| s.session == "code-splt0002");
        assert!(sb.is_some(), "a disconnected split brain must stay in the roster");
        assert_eq!(
            sb.unwrap().liveness,
            Liveness::Disconnected(DisconnectKind::SplitBrain)
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── M1/M2: baseline intent surfaced on the sibling roster ─────────────────

    #[test]
    fn live_siblings_carry_baseline_intent_when_no_todo() {
        // M2: a session that never emitted a todo still exposes its launch-task
        // intent (never blank). M1 renders this in the ambient note.
        let root = tmp_root();
        let wd = root.dir.join("co").display().to_string();
        std::fs::create_dir_all(&wd).unwrap();
        let me = std::process::id() as i32;
        sa2_record(&root, "code-view00001", &wd, "r", me);
        sa2_record(&root, "code-base0002", &wd, "r", me);
        set_intent(&root, "code-base0002", "harden the codex cage sandbox").unwrap();
        let sibs = live_siblings(&root, "code-view00001", &wd);
        let s = sibs.iter().find(|s| s.session == "code-base0002").unwrap();
        assert!(s.current_task.is_none(), "no todo was projected");
        assert_eq!(s.intent.as_deref(), Some("harden the codex cage sandbox"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn intent_launch_wins_and_first_prompt_seeds_only_when_absent() {
        // set_intent records the launch task; set_intent_if_absent (the interactive
        // first-prompt path) must NOT clobber it, but DOES seed when none exists.
        let root = tmp_root();
        sa2_record(&root, "code-int00001", "/tmp", "r", std::process::id() as i32);
        set_intent(&root, "code-int00001", "the launch task").unwrap();
        set_intent_if_absent(&root, "code-int00001", "a later prompt").unwrap();
        assert_eq!(
            get_intent(&root, "code-int00001").as_deref(),
            Some("the launch task"),
            "the launch task must win over a later prompt"
        );
        // With no prior intent, the first prompt seeds it.
        sa2_record(&root, "code-int00002", "/tmp", "r", std::process::id() as i32);
        set_intent_if_absent(&root, "code-int00002", "first prompt").unwrap();
        assert_eq!(
            get_intent(&root, "code-int00002").as_deref(),
            Some("first prompt")
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── M4: outcome classification + sitrep listing ───────────────────────────

    #[test]
    fn classify_outcome_folds_liveness_and_git() {
        use DisconnectKind::*;
        use Liveness::*;
        use Outcome::*;
        // A live (or split-brain) session is ALWAYS active — never declared removable
        // while it might still be running (the safety bias).
        assert_eq!(classify_outcome(Connected, true, false, false), Active);
        assert_eq!(
            classify_outcome(Disconnected(SplitBrain), true, false, false),
            Active
        );
        assert_eq!(
            classify_outcome(Disconnected(Unknown), true, true, true),
            Active
        );
        // A DEAD session whose branch merged with a clean tree = safe to remove.
        assert_eq!(classify_outcome(Dead, true, false, false), Merged);
        // A DEAD session with unmerged commits = WIP stranded (do not remove).
        assert_eq!(classify_outcome(Dead, false, true, false), WipStranded);
        // A DEAD session with a dirty tree = WIP stranded, even if the branch merged.
        assert_eq!(classify_outcome(Dead, true, false, true), WipStranded);
        // A DEAD session, not merged, nothing unshipped, clean = abandoned leftover.
        assert_eq!(classify_outcome(Dead, false, false, false), Abandoned);
    }

    #[test]
    fn sitrep_lists_a_merged_dead_session_and_a_live_one() {
        // The acceptance shape: a dead session whose branch merged reads as safe to
        // remove; a live session reads active — WITHOUT git archaeology (the git
        // outcome fold is the CLI's; here we assert the ledger view + liveness).
        let root = tmp_root();
        let wd = root.dir.join("co").display().to_string();
        std::fs::create_dir_all(&wd).unwrap();
        let live_pid = std::process::id() as i32;
        let dead_pid = 0x7fff_fffe; // gone on any sane host

        // A live session on branch feature-x with an intent.
        sa2_record(&root, "code-live00001", &wd, "r", live_pid);
        set_branch(&root, "code-live00001", "feature-x").unwrap();
        set_intent(&root, "code-live00001", "ship feature x").unwrap();

        // A confirmed-dead session on a merged branch.
        sa2_record(&root, "code-dead00002", &wd, "r", dead_pid);
        set_branch(&root, "code-dead00002", "wip-merged").unwrap();

        let rows = sitrep_sessions(&root);
        let live = rows.iter().find(|s| s.session == "code-live00001").unwrap();
        assert_eq!(live.liveness, Liveness::Connected);
        assert_eq!(live.branch.as_deref(), Some("feature-x"));
        assert_eq!(live.intent.as_deref(), Some("ship feature x"));
        // active, since not dead — regardless of git.
        assert_eq!(
            classify_outcome(live.liveness, false, false, false),
            Outcome::Active
        );

        let dead = rows.iter().find(|s| s.session == "code-dead00002").unwrap();
        assert_eq!(dead.liveness, Liveness::Dead);
        assert_eq!(dead.branch.as_deref(), Some("wip-merged"));
        // A dead + merged + clean session → safe to remove.
        let outcome = classify_outcome(dead.liveness, true, false, false);
        assert_eq!(outcome, Outcome::Merged);
        assert!(outcome.label().contains("safe to remove"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn session_liveness_reads_dead_and_live_from_the_ledger() {
        // The `ask` pre-check and `watch` read one named session's liveness. A gone
        // pid → Dead; a live pid → Connected; an unknown id → None.
        let root = tmp_root();
        let live_pid = std::process::id() as i32;
        let dead_pid = 0x7fff_fffe;
        sa2_record(&root, "code-l00000001", "/tmp", "r", live_pid);
        sa2_record(&root, "code-d00000002", "/tmp", "r", dead_pid);
        assert_eq!(
            session_liveness(&root, "code-l00000001"),
            Some(Liveness::Connected)
        );
        assert_eq!(session_liveness(&root, "code-d00000002"), Some(Liveness::Dead));
        assert_eq!(session_liveness(&root, "code-nosuch999"), None);
        let _ = std::fs::remove_dir_all(&root.dir);
    }
}
