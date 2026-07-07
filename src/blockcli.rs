//! `lanius block set/get/list/append/rm <name>` — the universal write surface
//! for memory blocks (docs/handoffs/memory-blocks.md M2). The CLI is the API:
//! any harness shells out to it exactly like `lanius code note` does today; the
//! MCP wrapper (handoff decision 1) is a deferred ergonomic upgrade.
//!
//! owner = the caller identity. For an agent's own blocks (scope=agent) the
//! owner is the profile's agent noun; `--owner` overrides for a human/package
//! writer. Multi-writer is owner-scoped, not locked (handoff decision 4): a peer
//! writing "your" block writes a DIFFERENT owner row.
//!
//! IDENTITY MODEL — `--owner` is a *self-attested label*, not an authenticated
//! identity. The `lanius block ...` CLI is a local-trusted surface (a harness
//! shells out to it exactly like `lanius code note`), so there is no broker
//! session to verify against; the owner string is taken at face value. This is
//! sound under lanius's homogeneous-authority doctrine (handoff decision 4 — "no
//! trust boundary between an owner's own agents"): a mismatched `--owner` only
//! ever writes a DIFFERENT owner row, which is invisible to and cannot overwrite
//! another owner's blocks (owner is part of the `context_blocks` key). It is an
//! attribution label, not an access-control boundary. When the same identity is
//! established by the broker elsewhere (principal=identity), that path verifies;
//! this hand-write path deliberately does not.

use crate::context_blocks::Placement;
use crate::context_blocks::{ContextBlock, Scope};
use crate::context_store::{self, parse_placement, parse_scope, placement_str, scope_str};
use crate::db;
use crate::paths::Root;
use crate::profile;
use anyhow::Result;
use serde_json::json;

/// Common block-addressing options shared by every verb.
pub struct BlockOpts {
    pub profile: String,
    pub session: String,
    pub scope: String,
    pub placement: String,
    pub priority: Option<i32>,
    pub owner: Option<String>,
    /// Decided-by attribution: who drove this write. The web UI passes `ui` so a
    /// human edit through `POST /api/blocks` is attributable in `context_build_log`
    /// (mirroring the `--by ui` trail every `/api/admin` mutation stamps). `None`
    /// for a plain agent/CLI write — the build log already records the owner/agent.
    pub by: Option<String>,
    /// Free-JSON `meta` for the block (kb-core.md M3). A KB pointer block carries
    /// `{ "kb": "<pkg>", "path": "kb/role-verifier.md", "lines": "12-28",
    /// "sha": "<content-sha256>" }` here; a plain block leaves it `None` (empty
    /// object). Must be a JSON object when present.
    pub meta: Option<String>,
}

/// Resolve the owner identity for a write: explicit `--owner` (a self-attested
/// label — see the module docs; the CLI is local-trusted and cannot verify it
/// against a broker session), else the profile's agent noun (the agent owns its
/// own scope=agent blocks). A mismatched label only writes a different owner row,
/// never crosses an owner boundary, so it stays within homogeneous-authority.
fn resolve_owner(root: &Root, opts: &BlockOpts) -> Result<String> {
    if let Some(o) = &opts.owner {
        return Ok(o.clone());
    }
    let (prof, _) = profile::load(root, &opts.profile)?;
    Ok(prof.agent)
}

fn run_binding(scope: &Scope) -> Option<&'static str> {
    // The CLI does not address `run` scope (no run id on the command line); a
    // run-scoped block is written by a stage inside a run, not by hand.
    match scope {
        Scope::Run => Some("(run-scope blocks are written inside a run, not via the CLI)"),
        _ => None,
    }
}

fn build_block(root: &Root, name: &str, content: &str, opts: &BlockOpts) -> Result<ContextBlock> {
    let scope = parse_scope(&opts.scope)?;
    if let Some(msg) = run_binding(&scope) {
        anyhow::bail!("{msg}");
    }
    let placement = parse_placement(&opts.placement)?;
    let owner = resolve_owner(root, opts)?;
    let mut b = ContextBlock::new(name, content, owner);
    b.scope = scope;
    b.placement = placement;
    if let Some(p) = opts.priority {
        b.priority = p;
    }
    if let Some(m) = &opts.meta {
        let parsed: serde_json::Value = serde_json::from_str(m)
            .map_err(|e| anyhow::anyhow!("--meta must be valid JSON: {e}"))?;
        if !parsed.is_object() {
            anyhow::bail!("--meta must be a JSON object (e.g. a kb pointer)");
        }
        b.meta = parsed;
    }
    Ok(b)
}

pub const USER_PLACEMENT_NOTE: &str = "note: `user` placement re-sends this block every turn; prefer `system` unless it changes each turn";

fn warn_user_placement(block: &ContextBlock) {
    if block.placement == Placement::User {
        eprintln!("{USER_PLACEMENT_NOTE}");
    }
}

/// `lanius block set <name> <content>`: upsert a block (last-writer-wins).
pub fn set(root: &Root, name: &str, content: &str, opts: &BlockOpts) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let block = build_block(root, name, content, opts)?;
    warn_user_placement(&block);
    let action = context_store::upsert_block(&conn, &opts.profile, &block, &opts.session, None)?;
    // Decided-by attribution (e.g. `--by ui`): record a `validate`-action build-log
    // row whose summary names the driver, so a human edit through the web UI is
    // attributable in `context_build_log` next to the upsert it accompanies. A bare
    // agent/CLI write passes `None` and skips this — the upsert row already carries
    // owner/agent. The summary holds only the attribution label, never block content.
    if let Some(by) = &opts.by {
        let summary = format!("by {by}");
        context_store::write_build_log(
            &conn,
            &opts.profile,
            &block.owner,
            &opts.session,
            None,
            "block-cli",
            &crate::context_blocks::BuildAction::Validate,
            Some(name),
            None,
            None,
            Some(&summary),
        )?;
    }
    println!(
        "{:?} block {} (owner {}, scope {}, placement {}, priority {})",
        action,
        name,
        block.owner,
        scope_str(&block.scope),
        placement_str(&block.placement),
        block.priority
    );
    Ok(())
}

/// `lanius block append <name> <content>`: append to a block, creating it if
/// absent (a newline joins prior content). Useful for accumulating notes.
pub fn append(root: &Root, name: &str, content: &str, opts: &BlockOpts) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let mut block = build_block(root, name, content, opts)?;
    let requested_user_placement = block.placement == Placement::User;
    if let Some(existing) = context_store::get_block(&conn, &block, &opts.session, None)? {
        block.content = if existing.content.is_empty() {
            content.to_string()
        } else {
            format!("{}\n{}", existing.content, content)
        };
        // Preserve the prior placement/priority on append (only content grows).
        block.priority = opts.priority.unwrap_or(existing.priority);
        block.placement = existing.placement;
    }
    if requested_user_placement || block.placement == Placement::User {
        eprintln!("{USER_PLACEMENT_NOTE}");
    }
    context_store::upsert_block(&conn, &opts.profile, &block, &opts.session, None)?;
    println!("appended to block {name}");
    Ok(())
}

/// `lanius block get <name>`: print one block's content, or exit non-zero.
pub fn get(root: &Root, name: &str, opts: &BlockOpts) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let block = build_block(root, name, "", opts)?;
    match context_store::get_block(&conn, &block, &opts.session, None)? {
        Some(b) => println!("{}", b.content),
        None => anyhow::bail!("no block {name:?} for owner {}", block.owner),
    }
    Ok(())
}

/// `lanius block list`: the system-placement blocks visible to this profile,
/// one JSON line each (so the web UI can consume it) in render order.
pub fn list(root: &Root, opts: &BlockOpts) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let (prof, _) = profile::load(root, &opts.profile)?;
    for b in context_store::load_system_blocks(&conn, &prof, &opts.session)? {
        println!(
            "{}",
            json!({
                "name": b.name,
                "owner": b.owner,
                "scope": scope_str(&b.scope),
                "placement": placement_str(&b.placement),
                "priority": b.priority,
                "content": b.content,
            })
        );
    }
    Ok(())
}

/// `lanius block rm <name>`: remove a block (logs the removal).
pub fn rm(root: &Root, name: &str, opts: &BlockOpts) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let block = build_block(root, name, "", opts)?;
    if context_store::remove_block(&conn, &opts.profile, &block, &opts.session, None)? {
        println!("removed block {name}");
    } else {
        println!("no block {name} to remove");
    }
    Ok(())
}

impl Default for BlockOpts {
    fn default() -> Self {
        BlockOpts {
            profile: "default".into(),
            session: "render-preview".into(),
            scope: "agent".into(),
            placement: "system".into(),
            priority: None,
            owner: None,
            by: None,
            meta: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-blockcli-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    // M3: `lanius block set --meta <json>` stores a machine-readable pointer.
    #[test]
    fn set_accepts_meta_json_and_round_trips() {
        let root = scratch("meta");
        let opts = BlockOpts {
            owner: Some("architect".into()), // explicit owner: no profile load
            meta: Some(
                r#"{"kb":"kb-llm-strengths","path":"kb/role-verifier.md","lines":"1-26","sha":"abc"}"#
                    .into(),
            ),
            ..Default::default()
        };
        set(&root, "kb-ptr", "Pick by role.", &opts).unwrap();

        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        let b = build_block(&root, "kb-ptr", "", &opts).unwrap();
        let meta = context_store::get_block_meta(&conn, &b, &opts.session, None)
            .unwrap()
            .unwrap();
        assert_eq!(meta["kb"], "kb-llm-strengths");
        assert_eq!(meta["path"], "kb/role-verifier.md");
        assert_eq!(meta["lines"], "1-26");

        // Invalid / non-object meta is refused, not silently dropped.
        let bad = BlockOpts {
            owner: Some("architect".into()),
            meta: Some("not json".into()),
            ..Default::default()
        };
        assert!(set(&root, "x", "y", &bad).is_err());
        let non_object = BlockOpts {
            owner: Some("architect".into()),
            meta: Some("[1,2,3]".into()),
            ..Default::default()
        };
        assert!(set(&root, "x", "y", &non_object).is_err());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn user_placement_note_states_the_tradeoff() {
        assert!(USER_PLACEMENT_NOTE.contains("re-sends this block every turn"));
        assert!(USER_PLACEMENT_NOTE.contains("prefer `system`"));
        assert!(USER_PLACEMENT_NOTE.contains("changes each turn"));
    }

    #[test]
    fn append_preserves_existing_user_placement_when_flag_omitted() {
        let root = scratch("append-user-placement");
        let user_opts = BlockOpts {
            owner: Some("architect".into()),
            placement: "user".into(),
            ..Default::default()
        };
        set(&root, "scratch", "one", &user_opts).unwrap();

        let default_opts = BlockOpts {
            owner: Some("architect".into()),
            ..Default::default()
        };
        append(&root, "scratch", "two", &default_opts).unwrap();

        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        let b = build_block(&root, "scratch", "", &default_opts).unwrap();
        let stored = context_store::get_block(&conn, &b, &default_opts.session, None)
            .unwrap()
            .unwrap();
        assert_eq!(stored.content, "one\ntwo");
        assert_eq!(stored.placement, Placement::User);
        std::fs::remove_dir_all(&root.dir).ok();
    }
}
