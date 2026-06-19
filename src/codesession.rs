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
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};

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
/// lease reaper (dispatcher::release_dead_leases) tests a holder pid.
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
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
}
