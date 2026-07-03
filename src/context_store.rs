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
//! M4 adds the coding-agent projection surface here: `load_session_blocks`
//! (blocks visible to a coding session keyed by agent noun + session, no Profile),
//! the `note`-block alias (`set_session_note`/`get_session_note`, M-decision 5 —
//! the memory note IS a session-scope block now), and the mid-cycle vector
//! (`is_mid_cycle`/`take_pending_mid_cycle`, content-addressed dedup). The
//! `turn_injection`/hook wiring that consumes these lives in `src/codeagent.rs`.
//! `code_notes` remains in the schema as legacy but nothing live reads it.

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
/// to `session`; a `run` block to a session+run. Unbound columns bind the empty
/// SENTINEL `''`, never NULL (storage-hardening M1): SQLite treats NULL as
/// DISTINCT in a UNIQUE index, so NULL-bound columns silently neutered the
/// `UNIQUE(scope, owner, session_id, run_id, name)` constraint and let
/// concurrent writers duplicate the same logical key. With `''` the constraint
/// fires and the upsert is one native `ON CONFLICT`. The binding is
/// deterministic per scope so a block round-trips to the same row.
fn scope_binding<'a>(scope: &Scope, session: &'a str, run: Option<&'a str>) -> (&'a str, &'a str) {
    match scope {
        Scope::Global | Scope::Agent => ("", ""),
        Scope::Session => (session, ""),
        Scope::Run => (session, run.unwrap_or("")),
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
    // Dedup-on-read guard (storage-hardening M2): collapse to one row per logical
    // key (max `id` wins) BEFORE ordering, so a DB restored from a pre-migration
    // backup — or an attach of an old `elanus.db` with NULL-keyed duplicates —
    // never renders the same block 2–30 times into the prompt. The sentinel
    // migration already prevents new duplicates; this is the belt-and-suspenders.
    // The visibility predicate matches the `''` sentinel, this session, AND legacy
    // NULL (an un-migrated row) so nothing silently disappears pre-migration.
    let mut stmt = conn.prepare(
        "SELECT name, content, priority, placement, owner, scope FROM (
           SELECT id, name, content, priority, placement, owner, scope,
                  ROW_NUMBER() OVER (
                    PARTITION BY scope, owner, session_id, run_id, name
                    ORDER BY id DESC
                  ) AS rn
             FROM context_blocks
            WHERE placement = 'system'
              AND owner IN (?1, ?2)
              AND (session_id = '' OR session_id IS NULL OR session_id = ?3)
         ) WHERE rn = 1
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

/// The well-known name of the per-session memory note block (M2 decision 5). The
/// memory note `elanus code note` writes/reads is just a session-scope block under
/// this name — one substrate, no separate `code_notes` read path in the live
/// injection.
pub const NOTE_BLOCK: &str = "note";

/// M4 — load the blocks visible to a CODING session, which is keyed by its agent
/// noun + session id rather than a full `Profile` (a coding agent has no profile
/// document; its identity is the agent noun the launcher recorded). Returns the
/// `system`-placement blocks owned by `owner` (the agent noun) that are either
/// unbound (agent/global scope) OR bound to THIS session, ordered by `priority`
/// (then name, then id) — the same order `load_system_blocks` uses for profiles.
/// This is the next-turn projection's source: the session's own agent-scope and
/// session-scope blocks, nobody else's.
pub fn load_session_blocks(
    conn: &Connection,
    owner: &str,
    session: &str,
) -> Result<Vec<LoadedBlock>> {
    // Dedup-on-read guard (storage-hardening M2) — see `load_system_blocks`.
    let mut stmt = conn.prepare(
        "SELECT name, content, priority, placement, owner, scope FROM (
           SELECT id, name, content, priority, placement, owner, scope,
                  ROW_NUMBER() OVER (
                    PARTITION BY scope, owner, session_id, run_id, name
                    ORDER BY id DESC
                  ) AS rn
             FROM context_blocks
            WHERE placement = 'system'
              AND owner = ?1
              AND (session_id = '' OR session_id IS NULL OR session_id = ?2)
         ) WHERE rn = 1
         ORDER BY priority ASC, name ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![owner, session], |r| {
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

/// Build the well-known `note` block for a coding session: session-scope, owned by
/// the session's agent noun, at a neutral priority. The single home for both the
/// `set`/`get` alias and the next-turn projection so the memory note IS a block.
fn note_block(owner: &str, content: &str) -> ContextBlock {
    let mut b = ContextBlock::new(NOTE_BLOCK, content, owner);
    b.scope = Scope::Session;
    b.placement = Placement::System;
    b
}

/// M2 decision 5 — write the per-session memory note as the well-known `note`
/// block (session scope, owner = agent noun). A blank note CLEARS it (removes the
/// block), preserving today's `elanus code note <session> ""` behavior. This is the
/// alias `set_note` re-points at: identical semantics, one substrate.
pub fn set_session_note(
    conn: &Connection,
    profile: &str,
    owner: &str,
    session: &str,
    note: &str,
) -> Result<()> {
    let note = note.trim();
    let block = note_block(owner, note);
    if note.is_empty() {
        remove_block(conn, profile, &block, session, None)?;
        return Ok(());
    }
    upsert_block(conn, profile, &block, session, None)?;
    Ok(())
}

/// M2 decision 5 — read the per-session memory note back out of the `note` block.
/// `None` when no note is set (the per-turn injection then omits the note line).
pub fn get_session_note(conn: &Connection, owner: &str, session: &str) -> Result<Option<String>> {
    let block = note_block(owner, "");
    Ok(get_block(conn, &block, session, None)?.map(|b| b.content))
}

/// The priority threshold at or below which a block is a MID-CYCLE block (M4). A
/// block qualifies for the mid-cycle injection vector — delivered between tool
/// calls, not just on the next turn — when its `priority <= MID_CYCLE_PRIORITY`.
///
/// Priority orders blocks ASCENDING (smaller = earlier/more important; see
/// `load_system_blocks` `ORDER BY priority ASC`), so "high priority" is a LOW
/// number. A normal block (the default `priority = 0`) is NOT mid-cycle — it lands
/// next-turn. A deliberately elevated block (`priority < 0`, e.g. an urgent note a
/// planner pushes mid-run) is mid-cycle. This keeps the common case (notes,
/// identity) next-turn and reserves the louder vector for explicitly-prioritized
/// content.
pub const MID_CYCLE_PRIORITY: i32 = -1;

/// Whether a loaded block qualifies for the mid-cycle vector (see
/// `MID_CYCLE_PRIORITY`).
pub fn is_mid_cycle(b: &LoadedBlock) -> bool {
    b.priority <= MID_CYCLE_PRIORITY
}

/// M4 mid-cycle vector — find the mid-cycle blocks (see `is_mid_cycle`) visible to
/// this coding session that have NOT yet been delivered mid-cycle at their current
/// content, mark them delivered, and return them. The dedup is content-addressed
/// (`code_block_delivered` keyed by `(session, block_name)`, carrying the delivered
/// `content_sha256`): an unchanged block is returned ONCE and not again on the next
/// tool call; editing the block changes its sha and re-arms a single redelivery.
/// The `note` block is excluded — it rides the next-turn vector (`[elanus note]`),
/// not the louder mid-cycle one, regardless of priority.
///
/// Caller contract: this MUTATES the dedup table (records delivery) — call it only
/// when actually about to emit the block (the Claude Code Pre/PostToolUse hook),
/// never as a pure read, or a block would be marked delivered without being shown.
pub fn take_pending_mid_cycle(
    conn: &Connection,
    owner: &str,
    session: &str,
) -> Result<Vec<LoadedBlock>> {
    let visible = load_session_blocks(conn, owner, session)?;
    let mut out = Vec::new();
    for b in visible {
        if b.name == NOTE_BLOCK || !is_mid_cycle(&b) {
            continue;
        }
        let sha = sha256_hex(b.content.as_bytes());
        // Already delivered at THIS exact content? (same name + sha) → skip.
        let already: bool = conn
            .query_row(
                "SELECT 1 FROM code_block_delivered
              WHERE session = ?1 AND block_name = ?2 AND content_sha256 = ?3",
                params![session, b.name, sha],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if already {
            continue;
        }
        // Record the delivery (last content wins per (session, block_name)) and emit.
        conn.execute(
            "INSERT INTO code_block_delivered (session, block_name, content_sha256)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session, block_name) DO UPDATE SET
               content_sha256 = excluded.content_sha256,
               delivered_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            params![session, b.name, sha],
        )?;
        out.push(b);
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
                AND session_id = ?3 AND run_id = ?4 AND name = ?5",
            params![scope_str(&block.scope), block.owner, sid, rid, block.name],
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

/// Read a block's `meta` JSON back out (kb-core.md M3 — `LoadedBlock` carries no
/// meta, so this is the focused reader that resolves a pointer block's
/// `{kb,path,lines,sha}`). `None` when the block does not exist; an empty object
/// for a block with no meta.
pub fn get_block_meta(
    conn: &Connection,
    block: &ContextBlock,
    session: &str,
    run: Option<&str>,
) -> Result<Option<serde_json::Value>> {
    let (sid, rid) = scope_binding(&block.scope, session, run);
    let raw: Option<String> = conn
        .query_row(
            "SELECT meta FROM context_blocks
              WHERE scope = ?1 AND owner = ?2
                AND session_id = ?3 AND run_id = ?4 AND name = ?5",
            params![scope_str(&block.scope), block.owner, sid, rid, block.name],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw.map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null)))
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
          WHERE scope = ?1 AND owner = ?2 AND session_id = ?3 AND run_id = ?4 AND name = ?5",
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
    let sha = block.content_sha256();
    let meta_json = serde_json::to_string(&block.meta).unwrap_or_else(|_| "{}".into());
    // One `BEGIN IMMEDIATE` transaction (storage-hardening M1): the sentinel
    // binding (`''` not NULL for unbound session_id/run_id, see `scope_binding`)
    // makes the `UNIQUE(scope, owner, session_id, run_id, name)` constraint fire,
    // so the upsert is one native `INSERT … ON CONFLICT … DO UPDATE` — atomic in
    // the engine, no read-then-branch TOCTOU window. IMMEDIATE takes the write
    // lock up front so concurrent writers to the same key serialize (busy_timeout
    // absorbs the wait) rather than racing. The `before` read and the
    // `context_build_log` row commit in the SAME transaction, so a crash between
    // the write and its log leaves neither.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<BuildAction> {
        let before = get_block(conn, block, session, run)?;
        conn.execute(
            "INSERT INTO context_blocks
               (scope, owner, session_id, run_id, name, placement, priority, package, content, content_sha256, meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(scope, owner, session_id, run_id, name) DO UPDATE SET
               placement = excluded.placement,
               priority = excluded.priority,
               package = excluded.package,
               content = excluded.content,
               content_sha256 = excluded.content_sha256,
               meta = excluded.meta,
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
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
    })();
    match result {
        Ok(action) => {
            conn.execute_batch("COMMIT")?;
            Ok(action)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
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
          WHERE scope = ?1 AND owner = ?2 AND session_id = ?3 AND run_id = ?4 AND name = ?5",
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
    defaults: &[(String, String, i32, serde_json::Value)],
    profile: &str,
    session: &str,
) -> Result<Vec<String>> {
    let mut seeded = Vec::new();
    for (name, content, priority, meta) in defaults {
        let mut block = ContextBlock::new(name, content, &prof.agent);
        block.scope = Scope::Agent;
        block.placement = Placement::System;
        block.priority = *priority;
        // A KB pointer block (kb-core.md M3) ships `meta = {kb,path,lines,sha}`;
        // an ordinary default ships an empty object. Seed-once carries it in.
        block.meta = meta.clone();
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
        let dir = std::env::temp_dir().join(format!(
            "el-store-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
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
        upsert_block(
            &c,
            "default",
            &block("shared", "lily's", "lily"),
            "s1",
            None,
        )
        .unwrap();
        // A peer writing "the same" block writes a DIFFERENT owner row — no clash.
        upsert_block(
            &c,
            "default",
            &block("shared", "scout's", "scout"),
            "s1",
            None,
        )
        .unwrap();
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
        let defaults = vec![(
            "identity".to_string(),
            "default text".to_string(),
            0,
            serde_json::json!({}),
        )];
        let seeded = seed_defaults(&c, &p, &defaults, "default", "s1").unwrap();
        assert_eq!(seeded, vec!["identity".to_string()]);
        // Evolve it.
        upsert_block(
            &c,
            "default",
            &block("identity", "evolved", "lily"),
            "s1",
            None,
        )
        .unwrap();
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
    fn note_alias_round_trips_as_a_block() {
        // M2 decision 5: the note IS a session-scope `note` block.
        let c = conn();
        assert!(get_session_note(&c, "claude-code", "code-n1")
            .unwrap()
            .is_none());
        set_session_note(
            &c,
            "default",
            "claude-code",
            "code-n1",
            "  do the migration  ",
        )
        .unwrap();
        // Trimmed, like the old code_notes path.
        assert_eq!(
            get_session_note(&c, "claude-code", "code-n1")
                .unwrap()
                .as_deref(),
            Some("do the migration")
        );
        // It is a real `note` block under the session.
        let blocks = load_session_blocks(&c, "claude-code", "code-n1").unwrap();
        assert!(blocks
            .iter()
            .any(|b| b.name == NOTE_BLOCK && b.content == "do the migration"));
        // A blank note clears it (removes the block).
        set_session_note(&c, "default", "claude-code", "code-n1", "   ").unwrap();
        assert!(get_session_note(&c, "claude-code", "code-n1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn load_session_blocks_is_owner_and_session_scoped() {
        let c = conn();
        // An agent-scope block (no session) and a session-scope block, both owned by
        // claude-code, are visible to code-s1.
        let mut agentb = block("identity", "I am Lily.", "claude-code");
        agentb.scope = Scope::Agent;
        upsert_block(&c, "default", &agentb, "code-s1", None).unwrap();
        let mut sessb = ContextBlock::new("focus", "ship M4", "claude-code");
        sessb.scope = Scope::Session;
        upsert_block(&c, "default", &sessb, "code-s1", None).unwrap();
        // A DIFFERENT session's session-block is not visible.
        let mut other = ContextBlock::new("focus", "other work", "claude-code");
        other.scope = Scope::Session;
        upsert_block(&c, "default", &other, "code-s2", None).unwrap();
        // A peer harness's block is not visible (owner filter).
        upsert_block(
            &c,
            "default",
            &block("identity", "peer", "codex"),
            "code-s1",
            None,
        )
        .unwrap();

        let names: Vec<String> = load_session_blocks(&c, "claude-code", "code-s1")
            .unwrap()
            .into_iter()
            .map(|b| b.content)
            .collect();
        assert!(names.contains(&"I am Lily.".to_string()));
        assert!(names.contains(&"ship M4".to_string()));
        assert!(
            !names.contains(&"other work".to_string()),
            "s2's block leaked into s1"
        );
        assert!(
            !names.contains(&"peer".to_string()),
            "codex's block leaked to claude-code"
        );
    }

    #[test]
    fn mid_cycle_is_priority_gated() {
        let mut normal = block("n", "x", "claude-code");
        normal.priority = 0;
        let loaded_normal = LoadedBlock {
            name: normal.name.clone(),
            content: normal.content.clone(),
            priority: normal.priority,
            placement: Placement::System,
            owner: normal.owner.clone(),
            scope: Scope::Agent,
        };
        assert!(
            !is_mid_cycle(&loaded_normal),
            "priority 0 is next-turn, not mid-cycle"
        );
        let mut hi = loaded_normal.clone();
        hi.priority = MID_CYCLE_PRIORITY;
        assert!(
            is_mid_cycle(&hi),
            "priority <= MID_CYCLE_PRIORITY is mid-cycle"
        );
    }

    #[test]
    fn take_pending_mid_cycle_dedups_until_content_changes() {
        let c = conn();
        // A high-priority (mid-cycle) session block.
        let mut b = ContextBlock::new("alert", "STOP: API changed", "claude-code");
        b.scope = Scope::Session;
        b.priority = MID_CYCLE_PRIORITY;
        upsert_block(&c, "default", &b, "code-mc1", None).unwrap();

        // First take: delivered once.
        let first = take_pending_mid_cycle(&c, "claude-code", "code-mc1").unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].name, "alert");

        // Second take (unchanged): NOT redelivered.
        let second = take_pending_mid_cycle(&c, "claude-code", "code-mc1").unwrap();
        assert!(
            second.is_empty(),
            "an unchanged mid-cycle block must not re-inject"
        );

        // Edit the block → its sha changes → re-armed once.
        let mut b2 = b.clone();
        b2.content = "STOP: API changed AGAIN".into();
        upsert_block(&c, "default", &b2, "code-mc1", None).unwrap();
        let third = take_pending_mid_cycle(&c, "claude-code", "code-mc1").unwrap();
        assert_eq!(
            third.len(),
            1,
            "editing the block re-arms a single redelivery"
        );

        // A NORMAL-priority block never rides the mid-cycle vector.
        upsert_block(
            &c,
            "default",
            &{
                let mut nb = ContextBlock::new("calm", "fyi", "claude-code");
                nb.scope = Scope::Session;
                nb // priority 0 default
            },
            "code-mc1",
            None,
        )
        .unwrap();
        // Drain the alert's re-arm first, then assert calm never appears.
        let _ = take_pending_mid_cycle(&c, "claude-code", "code-mc1").unwrap();
        let none = take_pending_mid_cycle(&c, "claude-code", "code-mc1").unwrap();
        assert!(
            none.iter().all(|x| x.name != "calm"),
            "a normal block must not go mid-cycle"
        );
    }

    #[test]
    fn note_block_never_rides_mid_cycle() {
        let c = conn();
        // Even a note forced to mid-cycle priority stays on the next-turn vector.
        let mut n = ContextBlock::new(NOTE_BLOCK, "urgent note", "claude-code");
        n.scope = Scope::Session;
        n.priority = MID_CYCLE_PRIORITY;
        upsert_block(&c, "default", &n, "code-mc2", None).unwrap();
        let pending = take_pending_mid_cycle(&c, "claude-code", "code-mc2").unwrap();
        assert!(
            pending.iter().all(|b| b.name != NOTE_BLOCK),
            "note rides next-turn only"
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

    /// A fresh root+db with the schema initialized, shareable across threads by
    /// its path (each thread opens its OWN connection).
    fn shared_root(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!(
            "el-store-shared-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let root = Root { dir };
        let c = crate::db::open(&root).unwrap();
        crate::db::init_schema(&c).unwrap();
        root
    }

    // storage-hardening M1 acceptance: N concurrent writers, each on its OWN
    // connection, upsert the SAME agent-scope key. Against the old NULL-neutered
    // UNIQUE + non-transactional read-then-branch this produced 5–10 duplicate
    // rows; with the sentinel + native ON CONFLICT under BEGIN IMMEDIATE it must
    // leave EXACTLY one row, its content one of the writers', and surface no
    // error.
    #[test]
    fn concurrent_upsert_same_key_yields_exactly_one_row() {
        let root = shared_root("concurrent");
        let n = 10;
        let mut handles = Vec::new();
        for i in 0..n {
            let root = root.clone();
            handles.push(std::thread::spawn(move || -> Result<()> {
                let c = crate::db::open(&root)?;
                upsert_block(
                    &c,
                    "default",
                    &block("identity", &format!("writer-{i}"), "lily"),
                    "s1",
                    None,
                )?;
                Ok(())
            }));
        }
        for h in handles {
            // No writer surfaced an error (busy_timeout absorbs the queueing).
            h.join().unwrap().expect("a concurrent upsert errored");
        }
        let c = crate::db::open(&root).unwrap();
        let rows: i64 = c
            .query_row(
                "SELECT count(*) FROM context_blocks
                  WHERE scope='agent' AND owner='lily' AND name='identity'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1, "concurrent same-key writers must not duplicate");
        let content: String = c
            .query_row(
                "SELECT content FROM context_blocks
                  WHERE scope='agent' AND owner='lily' AND name='identity'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            content.starts_with("writer-"),
            "surviving content is one writer's payload, got {content:?}"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    // storage-hardening M1 acceptance: a DB carrying hand-inserted NULL-keyed
    // duplicates (what a pre-migration instance accumulated) heals on open —
    // exactly one row per logical key, keeping the latest content, all
    // session_id/run_id backfilled to the non-NULL sentinel.
    #[test]
    fn migration_dedupes_null_keyed_and_backfills_sentinel() {
        let c = conn();
        // Two agent-scope duplicates of the same logical key (NULL session/run
        // dodges the UNIQUE index), distinct updated_at — 'new' is later.
        c.execute(
            "INSERT INTO context_blocks
               (scope, owner, session_id, run_id, name, placement, priority, content, content_sha256, meta, updated_at)
             VALUES ('agent','lily',NULL,NULL,'identity','system',0,'old',?1,'{}','2020-01-01T00:00:00.000Z')",
            params![sha256_hex(b"old")],
        )
        .unwrap();
        c.execute(
            "INSERT INTO context_blocks
               (scope, owner, session_id, run_id, name, placement, priority, content, content_sha256, meta, updated_at)
             VALUES ('agent','lily',NULL,NULL,'identity','system',0,'new',?1,'{}','2020-01-02T00:00:00.000Z')",
            params![sha256_hex(b"new")],
        )
        .unwrap();
        // A session-scope row with a bound session but NULL run_id (the pre-fix
        // shape for session scope) must survive and get its run_id backfilled.
        c.execute(
            "INSERT INTO context_blocks
               (scope, owner, session_id, run_id, name, placement, priority, content, content_sha256, meta, updated_at)
             VALUES ('session','lily','s1',NULL,'note','system',0,'hi',?1,'{}','2020-01-01T00:00:00.000Z')",
            params![sha256_hex(b"hi")],
        )
        .unwrap();
        let before: i64 = c
            .query_row("SELECT count(*) FROM context_blocks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, 3, "constraint was inert for NULL-keyed rows");

        // Re-running init_schema fires the sentinel migration.
        crate::db::init_schema(&c).unwrap();

        let (content, sid, rid): (String, String, String) = c
            .query_row(
                "SELECT content, session_id, run_id FROM context_blocks
                  WHERE scope='agent' AND name='identity'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(content, "new", "keep-latest (greatest updated_at) wins");
        assert_eq!(sid, "", "unbound session_id backfilled to sentinel");
        assert_eq!(rid, "", "unbound run_id backfilled to sentinel");
        // The session row survived and its NULL run_id became the sentinel.
        let (srid,): (String,) = c
            .query_row(
                "SELECT run_id FROM context_blocks WHERE scope='session' AND name='note'",
                [],
                |r| Ok((r.get(0)?,)),
            )
            .unwrap();
        assert_eq!(srid, "");
        // No NULLs remain anywhere; the identity dupe collapsed to one.
        let nulls: i64 = c
            .query_row(
                "SELECT count(*) FROM context_blocks WHERE session_id IS NULL OR run_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(nulls, 0);
        let total: i64 = c
            .query_row("SELECT count(*) FROM context_blocks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2, "identity dupe collapsed; note survived");
        // Idempotent: running again heals nothing.
        crate::db::init_schema(&c).unwrap();
        let total2: i64 = c
            .query_row("SELECT count(*) FROM context_blocks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total2, 2);
    }

    // storage-hardening M2 acceptance: even with two rows for the same logical
    // key present (a pre-migration NULL-keyed duplicate a restored backup could
    // carry), both load paths return the block exactly once, with the max-`id`
    // (newest) content.
    #[test]
    fn load_paths_dedup_duplicate_logical_key() {
        let c = conn();
        // Two agent-scope 'identity' rows for owner 'lily', NULL-keyed so they
        // bypass the UNIQUE constraint; the higher id carries the newer content.
        for (content, sha) in [("first", sha256_hex(b"first")), ("second", sha256_hex(b"second"))] {
            c.execute(
                "INSERT INTO context_blocks
                   (scope, owner, session_id, run_id, name, placement, priority, content, content_sha256, meta)
                 VALUES ('agent','lily',NULL,NULL,'identity','system',0,?1,?2,'{}')",
                params![content, sha],
            )
            .unwrap();
        }
        // load_system_blocks (profile owner = lily's agent) returns 'identity' once.
        let sys = load_system_blocks(&c, &prof("lily", "owner"), "s1").unwrap();
        let idents: Vec<&LoadedBlock> = sys.iter().filter(|b| b.name == "identity").collect();
        assert_eq!(idents.len(), 1, "system load must collapse the duplicate");
        assert_eq!(idents[0].content, "second", "max-id (newest) content wins");
        // load_session_blocks (owner = lily) same guarantee.
        let sess = load_session_blocks(&c, "lily", "s1").unwrap();
        let idents2: Vec<&LoadedBlock> = sess.iter().filter(|b| b.name == "identity").collect();
        assert_eq!(idents2.len(), 1, "session load must collapse the duplicate");
        assert_eq!(idents2[0].content, "second");
    }
}
