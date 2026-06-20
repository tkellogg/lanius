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
           (elanus_session, native_session, tool, agent_noun, workdir)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(elanus_session) DO UPDATE SET
           native_session = excluded.native_session,
           tool           = excluded.tool,
           agent_noun     = excluded.agent_noun,
           workdir        = excluded.workdir,
           last_active    = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        rusqlite::params![
            rec.elanus_session,
            rec.native_session,
            rec.tool,
            rec.agent_noun,
            rec.workdir,
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
            "SELECT elanus_session, native_session, tool, agent_noun, workdir
             FROM code_sessions WHERE elanus_session = ?1",
            [elanus_session],
            |r| {
                Ok(SessionRecord {
                    elanus_session: r.get(0)?,
                    native_session: r.get(1)?,
                    tool: r.get(2)?,
                    agent_noun: r.get(3)?,
                    workdir: r.get(4)?,
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

/// Record a delivery's idempotency key as processed. Returns `true` if this is
/// the FIRST time the key is seen (the delivery should be driven), `false` if the
/// key was already recorded (a duplicate — the caller skips the resume as a clean
/// no-op). Atomic via `INSERT … ON CONFLICT DO NOTHING`, so two concurrent claims
/// of the same key cannot both win the race. Durable: survives a restart, so the
/// at-least-once replay is caught.
pub fn claim_delivery_key(
    root: &Root,
    key: &str,
    session: &str,
    event_id: i64,
) -> Result<bool> {
    let conn = crate::db::open(root).context("opening the ledger for the delivery key")?;
    crate::db::init_schema(&conn)?;
    let inserted = conn.execute(
        "INSERT INTO code_delivery_keys (idempotency_key, session, event_id)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(idempotency_key) DO NOTHING",
        rusqlite::params![key, session, event_id],
    )?;
    Ok(inserted == 1)
}

/// Has this delivery key already been processed? A read-only check (the claim
/// itself is `claim_delivery_key`). Best-effort: a db error reads as "not seen"
/// so a transient failure never silently drops a real delivery.
pub fn delivery_key_seen(root: &Root, key: &str) -> bool {
    let Ok(conn) = crate::db::open(root) else {
        return false;
    };
    conn.query_row(
        "SELECT 1 FROM code_delivery_keys WHERE idempotency_key = ?1",
        [key],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
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

/// One minted session token plus the structural scope the broker enforces for
/// it. Stored as JSON at `<root>/.secrets/code-sessions/<session>.json`.
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
    /// Publish filters this session may publish to (structural: its own obs
    /// subtree). Everything else is denied by the broker ACL.
    pub publish: Vec<String>,
    /// Subscribe filters this session may subscribe to. Empty today — a coding
    /// session needs to *emit* its record, not read the bus, so it gets no read
    /// authority at all (M2's inbox is a later, explicitly-granted capability).
    pub subscribe: Vec<String>,
}

impl SessionToken {
    /// May this session publish here? Structural scope only.
    pub fn may_publish(&self, topic_name: &str) -> bool {
        self.publish
            .iter()
            .any(|f| crate::topic::matches(f, topic_name))
    }
    /// May this session subscribe to this filter? Exact-filter match against the
    /// granted set (today: none).
    pub fn may_subscribe(&self, filter: &str) -> bool {
        self.subscribe.iter().any(|f| f == filter)
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

/// Mint a grant-scoped session token for `principal` publishing `agent`
/// telemetry. Writes the 0600 token file inside the fenced store and returns the
/// token (the launcher hands `.secret` to the child as ELANUS_BUS_TOKEN). The
/// scope is structural: publish only `obs/agent/<agent>/<session>/#`, subscribe
/// nothing.
pub fn mint(root: &Root, principal: &str, agent: &str, owner_pid: i32) -> Result<SessionToken> {
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
    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    // Structural scope: exactly the session's own obs subtree, encoded the same
    // way codeagent::obs_topic encodes the agent/session segments so the filter
    // and the published topics agree even for names with reserved characters.
    let own_obs = format!(
        "obs/agent/{}/{}/#",
        crate::topic::encode_segment(agent),
        crate::topic::encode_segment(principal),
    );
    let token = SessionToken {
        principal: principal.to_string(),
        agent: agent.to_string(),
        secret,
        owner_pid,
        publish: vec![own_obs],
        subscribe: Vec::new(),
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
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(contents.as_bytes())
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
        let tok = mint(&root, "code-deadbeef", "claude-code", 999_999).unwrap();
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
        let minted = mint(&root, "code-cafef00d", "claude-code", 1234).unwrap();
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
        mint(&root, "code-deadbeef", "claude-code", dead_pid).unwrap();
        let live = mint(
            &root,
            "code-livesess",
            "claude-code",
            std::process::id() as i32,
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
        // First claim of a key wins; the key is now seen.
        assert!(!delivery_key_seen(&root, "event:5"));
        assert!(claim_delivery_key(&root, "event:5", "code-x", 5).unwrap());
        assert!(delivery_key_seen(&root, "event:5"));
        // A second claim of the SAME key loses (a duplicate — the at-least-once
        // replay): it must NOT drive a second resume.
        assert!(!claim_delivery_key(&root, "event:5", "code-x", 5).unwrap());
        // A different key is independent.
        assert!(claim_delivery_key(&root, "planner-step-2", "code-y", 9).unwrap());
        // Durable across a fresh connection (a restart): the row is in the ledger,
        // so the replayed delivery is still recognized.
        assert!(delivery_key_seen(&root, "event:5"));
        assert!(delivery_key_seen(&root, "planner-step-2"));
        assert!(!delivery_key_seen(&root, "event:999"));
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
        };
        upsert_record(&root, &rec).unwrap();
        // Idle: record present, no token.
        assert!(read_record(&root, "code-resume01").unwrap().is_some());
        assert!(read(&root, "code-resume01").is_none());

        // Resume mints a fresh, emit-only token …
        let token = mint(&root, "code-resume01", "codex", std::process::id() as i32).unwrap();
        assert!(token.may_publish("obs/agent/codex/code-resume01/session/resume"));
        assert!(!token.may_publish("in/human/owner"));
        assert!(token.subscribe.is_empty(), "resume token must be emit-only");
        assert!(read(&root, "code-resume01").is_some());

        // … and retires it: no idle credential survives the resume.
        retire(&root, "code-resume01");
        assert!(read(&root, "code-resume01").is_none());
        // The durable record outlives the token — still resumable later.
        assert!(read_record(&root, "code-resume01").unwrap().is_some());
    }
}
