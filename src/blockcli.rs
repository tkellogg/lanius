//! `elanus block set/get/list/append/rm <name>` — the universal write surface
//! for memory blocks (docs/handoffs/memory-blocks.md M2). The CLI is the API:
//! any harness shells out to it exactly like `elanus code note` does today; the
//! MCP wrapper (handoff decision 1) is a deferred ergonomic upgrade.
//!
//! owner = the caller identity. For an agent's own blocks (scope=agent) the
//! owner is the profile's agent noun; `--owner` overrides for a human/package
//! writer. Multi-writer is owner-scoped, not locked (handoff decision 4): a peer
//! writing "your" block writes a DIFFERENT owner row.
//!
//! IDENTITY MODEL — `--owner` is a *self-attested label*, not an authenticated
//! identity. The `elanus block ...` CLI is a local-trusted surface (a harness
//! shells out to it exactly like `elanus code note`), so there is no broker
//! session to verify against; the owner string is taken at face value. This is
//! sound under elanus's homogeneous-authority doctrine (handoff decision 4 — "no
//! trust boundary between an owner's own agents"): a mismatched `--owner` only
//! ever writes a DIFFERENT owner row, which is invisible to and cannot overwrite
//! another owner's blocks (owner is part of the `context_blocks` key). It is an
//! attribution label, not an access-control boundary. When the same identity is
//! established by the broker elsewhere (principal=identity), that path verifies;
//! this hand-write path deliberately does not.

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
    Ok(b)
}

/// `elanus block set <name> <content>`: upsert a block (last-writer-wins).
pub fn set(root: &Root, name: &str, content: &str, opts: &BlockOpts) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let block = build_block(root, name, content, opts)?;
    let action = context_store::upsert_block(&conn, &opts.profile, &block, &opts.session, None)?;
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

/// `elanus block append <name> <content>`: append to a block, creating it if
/// absent (a newline joins prior content). Useful for accumulating notes.
pub fn append(root: &Root, name: &str, content: &str, opts: &BlockOpts) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let mut block = build_block(root, name, content, opts)?;
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
    context_store::upsert_block(&conn, &opts.profile, &block, &opts.session, None)?;
    println!("appended to block {name}");
    Ok(())
}

/// `elanus block get <name>`: print one block's content, or exit non-zero.
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

/// `elanus block list`: the system-placement blocks visible to this profile,
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

/// `elanus block rm <name>`: remove a block (logs the removal).
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
        }
    }
}
