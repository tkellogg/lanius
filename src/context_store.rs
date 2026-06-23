//! The durable read/write surface over the `context_blocks` table — the bridge
//! that turns the unwired substrate (`src/context_blocks.rs`,
//! `src/db.rs::context_blocks`) into live context (docs/handoffs/memory-blocks.md
//! M1–M3).
//!
//! A **memory block** is a named, durable chunk of prompt: `name -> content`,
//! keyed by `(scope, owner, session_id, run_id, name)`, ordered by `priority`,
//! placed by `placement`. M1 seeds the `system`-placement rows visible to a
//! `(scope, owner, session)` into the native Doc system seed; M2 is the write
//! surface (`elanus block …`) plus the seed-once "default that evolves"; M3
//! records every mutation in `context_build_log`.
//!
//! What this module does NOT do (deferred, by handoff): the coding-agent
//! projection / turn_injection wiring (M4 — `src/codeagent.rs` is off-limits)
//! and re-pointing `elanus code note` at a `note` block (note-aliasing, M-decision
//! 5). `code_notes` is left exactly as it is.

use crate::context_blocks::{sha256_hex, BuildAction, ContextBlock, Placement, Scope};
use crate::profile::Profile;
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

/// Parse the textual `scope` column / CLI flag into a `Scope`.
pub fn parse_scope(s: &str) -> Result<Scope> {
    Ok(match s {
        "global" => Scope::Global,
        "agent" => Scope::Agent,
        "session" => Scope::Session,
        "run" => Scope::Run,
        other => bail!("unknown scope {other:?} (global|agent|session|run)"),
    })
}

pub fn scope_str(s: &Scope) -> &'static str {
    match s {
        Scope::Global => "global",
        Scope::Agent => "agent",
        Scope::Session => "session",
        Scope::Run => "run",
    }
}

/// Parse the textual `placement` column / CLI flag into a `Placement`.
pub fn parse_placement(s: &str) -> Result<Placement> {
    Ok(match s {
        "system" => Placement::System,
        "before_messages" => Placement::BeforeMessages,
        "after_messages" => Placement::AfterMessages,
        "user" => Placement::User,
        "scratch" => Placement::Scratch,
        other => bail!(
            "unknown placement {other:?} (system|before_messages|after_messages|user|scratch)"
        ),
    })
}

pub fn placement_str(p: &Placement) -> &'static str {
    match p {
        Placement::System => "system",
        Placement::BeforeMessages => "before_messages",
        Placement::AfterMessages => "after_messages",
        Placement::User => "user",
        Placement::Scratch => "scratch",
    }
}

/// A durable block row loaded from the table, plus its priority/placement (the
/// `ContextBlock` value type does not round-trip priority through the seed path,
/// so we keep the loaded shape with everything the renderer needs to order it).
#[derive(Debug, Clone)]
pub struct LoadedBlock {
    pub name: String,
    pub content: String,
    pub priority: i32,
    pub placement: Placement,
    pub owner: String,
    pub scope: Scope,
}

/// The `session_id` / `run_id` columns a row carries for a given scope. A
/// `global`/`agent` block is not bound to a session; a `session` block is bound
/// to `session`; a `run` block to a session+run. The UNIQUE key includes both,
/// so the binding must be deterministic per scope to upsert and to read the
/// same row back.
fn scope_binding<'a>(
    scope: &Scope,
    session: &'a str,
    run: Option<&'a str>,
) -> (Option<&'a str>, Option<&'a str>) {
    match scope {
        Scope::Global | Scope::Agent => (None, None),
        Scope::Session => (Some(session), None),
        Scope::Run => (Some(session), run),
    }
}

/// M1 — load the `system`-placement blocks visible to `(scope, owner, session)`
/// for this profile, ordered by `priority` (then name for stability). Visibility
/// today is the homogeneous-authority slice: a block is visible if it is the
/// profile owner's or its agent's, and bound to no session OR to this session.
/// (Richer placements live in the table but have no Doc home yet — M1 honors
/// `system` only, per handoff decision 2.)
pub fn load_system_blocks(
    conn: &Connection,
    prof: &Profile,
    session: &str,
) -> Result<Vec<LoadedBlock>> {
    let mut stmt = conn.prepare(
        "SELECT name, content, priority, placement, owner, scope
           FROM context_blocks
          WHERE placement = 'system'
            AND owner IN (?1, ?2)
            AND (session_id IS NULL OR session_id = ?3)
          ORDER BY priority ASC, name ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![prof.agent, prof.owner, session], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i32>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (name, content, priority, placement, owner, scope) = row?;
        out.push(LoadedBlock {
            name,
            content,
            priority,
            placement: parse_placement(&placement).unwrap_or(Placement::System),
            owner,
            scope: parse_scope(&scope).unwrap_or(Scope::Agent),
        });
    }
    Ok(out)
}

/// Read one block by its full key, if present.
pub fn get_block(
    conn: &Connection,
    block: &ContextBlock,
    session: &str,
    run: Option<&str>,
) -> Result<Option<LoadedBlock>> {
    let (sid, rid) = scope_binding(&block.scope, session, run);
    let row = conn
        .query_row(
            "SELECT name, content, priority, placement, owner, scope
               FROM context_blocks
              WHERE scope = ?1 AND owner = ?2
                AND session_id IS ?3 AND run_id IS ?4 AND name = ?5",
            params![
                scope_str(&block.scope),
                block.owner,
                sid,
                rid,
                block.name
            ],
            |r| {
                Ok(LoadedBlock {
                    name: r.get(0)?,
                    content: r.get(1)?,
                    priority: r.get(2)?,
                    placement: Placement::System,
                    owner: r.get(4)?,
                    scope: Scope::Agent,
                })
            },
        )
        .optional()?;
    Ok(row.map(|mut lb| {
        // placement/scope re-parsed from the row's own strings (the closure
        // can't fallibly parse, so re-read here).
        lb.placement = block.placement.clone();
        lb.scope = block.scope.clone();
        lb
    }))
}

/// Whether a block row already exists for this exact key.
fn exists(
    conn: &Connection,
    block: &ContextBlock,
    session: &str,
    run: Option<&str>,
) -> Result<bool> {
    let (sid, rid) = scope_binding(&block.scope, session, run);
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM context_blocks
          WHERE scope = ?1 AND owner = ?2 AND session_id IS ?3 AND run_id IS ?4 AND name = ?5",
        params![scope_str(&block.scope), block.owner, sid, rid, block.name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// The `component` attribution for a build-log row: a package writes as its
/// package name, an agent/human writes as `cli:<owner>`. Kept legible for the
/// "which component added/edited which block" reconstruction (M3).
fn component_for(block: &ContextBlock) -> String {
    match &block.package {
        Some(pkg) => pkg.clone(),
        None => format!("cli:{}", block.owner),
    }
}

/// Upsert a block (M2) and record the mutation in `context_build_log` (M3).
/// Last-writer-wins per `(scope, owner, session_id, run_id, name)` — multi-writer
/// is owner-scoped, not locked (handoff decision 4). Returns the action taken.
pub fn upsert_block(
    conn: &Connection,
    profile: &str,
    block: &ContextBlock,
    session: &str,
    run: Option<&str>,
) -> Result<BuildAction> {
    block.validate()?;
    let (sid, rid) = scope_binding(&block.scope, session, run);
    let before = get_block(conn, block, session, run)?;
    let sha = block.content_sha256();
    let meta_json = serde_json::to_string(&block.meta).unwrap_or_else(|_| "{}".into());
    // NOTE: a plain `ON CONFLICT(scope, owner, session_id, run_id, name)` does
    // NOT fire for agent/global-scope rows, because SQLite treats NULL as
    // DISTINCT in a UNIQUE index — the unbound session_id/run_id are NULL, so two
    // "same key" writes would each insert a fresh row. We therefore resolve the
    // upsert by hand against the same `IS`-comparison key get_block/exists use:
    // UPDATE the existing row, else INSERT. (Last-writer-wins per key.)
    if before.is_some() {
        conn.execute(
            "UPDATE context_blocks SET
               placement = ?6, priority = ?7, package = ?8, content = ?9,
               content_sha256 = ?10, meta = ?11,
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE scope = ?1 AND owner = ?2 AND session_id IS ?3 AND run_id IS ?4 AND name = ?5",
            params![
                scope_str(&block.scope),
                block.owner,
                sid,
                rid,
                block.name,
                placement_str(&block.placement),
                block.priority,
                block.package,
                block.content,
                sha,
                meta_json,
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO context_blocks
               (scope, owner, session_id, run_id, name, placement, priority, package, content, content_sha256, meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                scope_str(&block.scope),
                block.owner,
                sid,
                rid,
                block.name,
                placement_str(&block.placement),
                block.priority,
                block.package,
                block.content,
                sha,
                meta_json,
            ],
        )?;
    }
    let action = if before.is_some() {
        BuildAction::Rewrite
    } else {
        BuildAction::Add
    };
    write_build_log(
        conn,
        profile,
        &block.owner,
        session,
        run,
        &component_for(block),
        &action,
        Some(&block.name),
        before.as_ref().map(|b| sha256_hex(b.content.as_bytes())),
        Some(sha),
        None,
    )?;
    Ok(action)
}

/// Remove a block (M2) and log the removal.
pub fn remove_block(
    conn: &Connection,
    profile: &str,
    block: &ContextBlock,
    session: &str,
    run: Option<&str>,
) -> Result<bool> {
    let (sid, rid) = scope_binding(&block.scope, session, run);
    let before = get_block(conn, block, session, run)?;
    let n = conn.execute(
        "DELETE FROM context_blocks
          WHERE scope = ?1 AND owner = ?2 AND session_id IS ?3 AND run_id IS ?4 AND name = ?5",
        params![scope_str(&block.scope), block.owner, sid, rid, block.name],
    )?;
    if let Some(b) = &before {
        write_build_log(
            conn,
            profile,
            &block.owner,
            session,
            run,
            &component_for(block),
            &BuildAction::Remove,
            Some(&block.name),
            Some(sha256_hex(b.content.as_bytes())),
            None,
            None,
        )?;
    }
    Ok(n > 0)
}

/// M2 — the "default that evolves" seed-once path. A profile that ships a
/// `blocks/<name>.md` (or a package a manifest default) declares a *fallback*
/// block. On first render, if no stored row exists for `(scope=agent, owner=<agent>,
/// name)`, seed one from the default; every read thereafter returns the stored
/// (possibly agent-edited) row. A later `set` wins and survives a re-render —
/// the default never overwrites. Returns the names seeded this call.
pub fn seed_defaults(
    conn: &Connection,
    prof: &Profile,
    defaults: &[(String, String, i32)],
    profile: &str,
    session: &str,
) -> Result<Vec<String>> {
    let mut seeded = Vec::new();
    for (name, content, priority) in defaults {
        let mut block = ContextBlock::new(name, content, &prof.agent);
        block.scope = Scope::Agent;
        block.placement = Placement::System;
        block.priority = *priority;
        if block.validate().is_err() {
            continue; // a malformed default name is a no-op, never an error
        }
        if exists(conn, &block, session, None)? {
            continue; // stored-wins: never overwrite an evolved value
        }
        upsert_block(conn, profile, &block, session, None)?;
        seeded.push(name.clone());
    }
    Ok(seeded)
}

/// Insert one `context_build_log` row (M3): enough to reconstruct which
/// component added/rewrote/removed which block. Summaries/hashes only, never the
/// full prompt document (the table's own doctrine, db.rs:308).
#[allow(clippy::too_many_arguments)]
pub fn write_build_log(
    conn: &Connection,
    profile: &str,
    agent: &str,
    session: &str,
    run: Option<&str>,
    component: &str,
    action: &BuildAction,
    block_name: Option<&str>,
    before_sha256: Option<String>,
    after_sha256: Option<String>,
    summary: Option<&str>,
) -> Result<()> {
    let action_str = match action {
        BuildAction::Add => "add",
        BuildAction::Remove => "remove",
        BuildAction::Rewrite => "rewrite",
        BuildAction::Drop => "drop",
        BuildAction::Move => "move",
        BuildAction::Validate => "validate",
    };
    conn.execute(
        "INSERT INTO context_build_log
           (session_id, run_id, profile, agent, component, action, block_name, before_sha256, after_sha256, summary, meta)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, '{}')",
        params![
            session,
            run,
            profile,
            agent,
            component,
            action_str,
            block_name,
            before_sha256,
            after_sha256,
            summary,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Root;

    fn conn() -> Connection {
        let dir = std::env::temp_dir().join(format!("el-store-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let root = Root { dir };
        let c = crate::db::open(&root).unwrap();
        crate::db::init_schema(&c).unwrap();
        c
    }

    fn prof(agent: &str, owner: &str) -> Profile {
        let toml = format!("agent = \"{agent}\"\nowner = \"{owner}\"\n");
        toml::from_str(&toml).unwrap()
    }

    fn block(name: &str, content: &str, owner: &str) -> ContextBlock {
        let mut b = ContextBlock::new(name, content, owner);
        b.scope = Scope::Agent;
        b
    }

    #[test]
    fn upsert_get_roundtrip_and_last_writer_wins() {
        let c = conn();
        let a = upsert_block(&c, "default", &block("identity", "one", "lily"), "s1", None).unwrap();
        assert_eq!(a, BuildAction::Add);
        let got = get_block(&c, &block("identity", "", "lily"), "s1", None)
            .unwrap()
            .unwrap();
        assert_eq!(got.content, "one");

        // A second write to the same key rewrites (last-writer-wins).
        let a2 =
            upsert_block(&c, "default", &block("identity", "two", "lily"), "s1", None).unwrap();
        assert_eq!(a2, BuildAction::Rewrite);
        let got = get_block(&c, &block("identity", "", "lily"), "s1", None)
            .unwrap()
            .unwrap();
        assert_eq!(got.content, "two");

        // The build log recorded the add and the rewrite.
        let n: i64 = c
            .query_row(
                "SELECT count(*) FROM context_build_log WHERE block_name='identity'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn multi_writer_is_owner_scoped_not_locked() {
        let c = conn();
        upsert_block(&c, "default", &block("shared", "lily's", "lily"), "s1", None).unwrap();
        // A peer writing "the same" block writes a DIFFERENT owner row — no clash.
        upsert_block(&c, "default", &block("shared", "scout's", "scout"), "s1", None).unwrap();
        assert_eq!(
            get_block(&c, &block("shared", "", "lily"), "s1", None)
                .unwrap()
                .unwrap()
                .content,
            "lily's"
        );
        assert_eq!(
            get_block(&c, &block("shared", "", "scout"), "s1", None)
                .unwrap()
                .unwrap()
                .content,
            "scout's"
        );
        // load_system_blocks (visibility) returns only the profile owner/agent's.
        let visible = load_system_blocks(&c, &prof("lily", "owner"), "s1").unwrap();
        assert!(visible.iter().any(|b| b.owner == "lily"));
        assert!(
            !visible.iter().any(|b| b.owner == "scout"),
            "a peer's row is not visible to lily's profile"
        );
    }

    #[test]
    fn remove_block_logs_and_deletes() {
        let c = conn();
        upsert_block(&c, "default", &block("tmp", "x", "lily"), "s1", None).unwrap();
        assert!(remove_block(&c, "default", &block("tmp", "", "lily"), "s1", None).unwrap());
        assert!(get_block(&c, &block("tmp", "", "lily"), "s1", None)
            .unwrap()
            .is_none());
        // Removing again is a no-op (false), no extra log row.
        assert!(!remove_block(&c, "default", &block("tmp", "", "lily"), "s1", None).unwrap());
        let removes: i64 = c
            .query_row(
                "SELECT count(*) FROM context_build_log WHERE action='remove'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(removes, 1);
    }

    #[test]
    fn seed_defaults_is_seed_once() {
        let c = conn();
        let p = prof("lily", "owner");
        let defaults = vec![("identity".to_string(), "default text".to_string(), 0)];
        let seeded = seed_defaults(&c, &p, &defaults, "default", "s1").unwrap();
        assert_eq!(seeded, vec!["identity".to_string()]);
        // Evolve it.
        upsert_block(&c, "default", &block("identity", "evolved", "lily"), "s1", None).unwrap();
        // A second seed never overwrites the evolved value.
        let seeded2 = seed_defaults(&c, &p, &defaults, "default", "s1").unwrap();
        assert!(seeded2.is_empty());
        assert_eq!(
            get_block(&c, &block("identity", "", "lily"), "s1", None)
                .unwrap()
                .unwrap()
                .content,
            "evolved"
        );
    }

    #[test]
    fn session_scope_binds_to_its_session() {
        let c = conn();
        let mut b = block("note", "for s1", "lily");
        b.scope = Scope::Session;
        upsert_block(&c, "default", &b, "s1", None).unwrap();
        // The same key under a different session is a different row.
        assert!(get_block(&c, &b, "s1", None).unwrap().is_some());
        assert!(get_block(&c, &b, "s2", None).unwrap().is_none());
    }
}
