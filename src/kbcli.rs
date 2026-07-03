//! `elanus kb list` / `elanus kb write` (docs/handoffs/kb-core.md). The CLI is the
//! API, the way `elanus config` and `elanus block` are: a harness shells out to it.
//! `list` names the enabled knowledge bases (packages carrying the `[kb]` marker);
//! `write` is the convenience verb that writes a file into a KB's `kb/` tree and
//! commits it atomically with the shared hardened-git discipline. The taught
//! pattern (so a default agent "just knows" how to write knowledge) lives in the
//! stdlib `knowledge` skill; this verb is the sharp edge behind it.

use crate::db;
use crate::events::{self, EmitOpts};
use crate::groundskeeper;
use crate::kb;
use crate::paths::Root;
use anyhow::Result;
use serde_json::json;

/// `elanus kb list [--json]`: the enabled KBs visible to `profile`, human table
/// or one JSON line each (for a harness to consume).
pub fn list(root: &Root, profile: &str, json_out: bool) -> Result<()> {
    let kbs = kb::enumerate(root, profile)?;
    if json_out {
        for k in &kbs {
            println!(
                "{}",
                json!({
                    "package": k.package,
                    "title": k.title,
                    "description": k.description,
                    "files": k.files,
                    "path": k.path.display().to_string(),
                })
            );
        }
        return Ok(());
    }
    if kbs.is_empty() {
        println!("no knowledge bases enabled (a package declares one with a [kb] marker)");
        return Ok(());
    }
    for k in &kbs {
        let title = k.title.as_deref().unwrap_or("(untitled)");
        println!(
            "{}  {}  [{} file{}]  {}",
            k.package,
            title,
            k.files,
            if k.files == 1 { "" } else { "s" },
            k.path.display()
        );
    }
    Ok(())
}

/// `elanus kb search <query>`: ranked file+line hits from the kb-search FTS5
/// index (the same index the `search_knowledge` tool reads, so identical hits).
/// Human list or one JSON line per hit.
pub fn search(root: &Root, query: &str, limit: usize, json_out: bool) -> Result<()> {
    let hits = kb::search(&kb::search_index_path(root), query, limit)?;
    if json_out {
        for h in &hits {
            println!(
                "{}",
                json!({
                    "package": h.package,
                    "path": h.path,
                    "lines": h.lines,
                    "snippet": h.snippet,
                })
            );
        }
        return Ok(());
    }
    if hits.is_empty() {
        println!("no hits for {query:?}");
        return Ok(());
    }
    for h in &hits {
        println!("{}/{}:{}  {}", h.package, h.path, h.lines, h.snippet);
    }
    Ok(())
}

/// `elanus kb write <pkg> <path>`: write stdin (or `--content`) into `kb/<path>`
/// and commit it. Write-then-commit is atomic with the KB's hardened git.
pub fn write(root: &Root, pkg: &str, path: &str, content: &str) -> Result<()> {
    let out = kb::write(root, pkg, path, content)?;
    if out.changed {
        println!(
            "wrote {} in {} — committed {}",
            out.rel,
            out.package,
            short(&out.commit)
        );
    } else {
        println!("{}/{} unchanged — no commit", out.package, out.rel);
    }
    Ok(())
}

/// `elanus kb check [--profile] [--json] [--mail]`: the M1 groundskeeper sweep
/// (docs/handoffs/kb-groundskeeper.md) — validate pointer blocks, find orphans,
/// flag staleness. ZERO LLM calls. With `--mail`, emit the report to the owner's
/// mailbox (in/human/owner) when there are findings — the cron sweep's deliverable.
pub fn check(root: &Root, profile: &str, json_out: bool, mail: bool) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let report = groundskeeper::sweep(root, &conn, profile)?;
    if json_out {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print!("{}", groundskeeper::report_summary(&report));
    }
    if mail && report.has_findings() {
        events::emit(
            root,
            &conn,
            EmitOpts {
                payload: Some(json!({
                    "text": groundskeeper::report_summary(&report),
                    "report": report,
                    "source": groundskeeper::PKG,
                })),
                ..EmitOpts::new("in/human/owner")
            },
        )?;
    }
    Ok(())
}

/// `elanus kb apply-diff <pkg>`: apply a unified diff (from stdin, or `--content`)
/// into a KB's `kb/` tree and commit exactly what it touches — the ratifier's
/// apply path (docs/handoffs/kb-groundskeeper.md M3). Path-disciplined: a diff that
/// reaches outside `kb/` is refused before any file is touched.
pub fn apply_diff(root: &Root, pkg: &str, diff: &str) -> Result<()> {
    let out = kb::apply_diff(root, pkg, diff)?;
    if out.changed {
        println!(
            "ratified {} in {} — committed {}",
            out.rel,
            out.package,
            short(&out.commit)
        );
    } else {
        println!("{} — diff applied no changes, no commit", out.package);
    }
    Ok(())
}

/// `elanus kb groundskeep [--profile]`: the pipeline dispatch (M3) the cron calls.
/// The absolute setup gate first: if the pipeline is not set up (config keys unset,
/// the package unapproved, OR the pipeline exec handler `kb-pipeline` unapproved so
/// the compactor mailbox is not daemon-drivable) it is INERT — it prints why and
/// spawns nothing. Once set up, it spawns the compactor (spawn_core) with the
/// configured cheap model + per-pass token budget threaded onto the payload; the
/// compactor's sweep and the ratifier round-trip are the live work (a documented
/// smoke step).
pub fn groundskeep(root: &Root, profile: &str) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    crate::packages::sync(root, &conn)?;
    let cfg = match groundskeeper::load_config(root)? {
        Ok(cfg) => cfg,
        Err(reason) => {
            println!("inert: {reason}");
            return Ok(());
        }
    };
    if !crate::packages::is_granted(&conn, groundskeeper::PKG)? {
        println!(
            "inert: {} is not approved (run `elanus approve {}`)",
            groundskeeper::PKG,
            groundskeeper::PKG
        );
        return Ok(());
    }
    // The compactor mailbox must have an approved exec handler or spawn_core would
    // refuse to launch (src/agentcli.rs). Treat a missing/unapproved handler as an
    // inert state with a clear reason rather than a hard error, so the hourly cron
    // kick stays quiet until the human finishes setup.
    if let Some(reason) = groundskeeper::handler_gap(root, &conn)? {
        println!("inert: {reason}");
        return Ok(());
    }
    let corpus = groundskeeper::corpus_digest(root, profile)?;
    let req = groundskeeper::compactor_request(&cfg, &corpus);
    let descriptor = crate::agentcli::spawn_core(root, &conn, req)?;
    println!("{descriptor}");
    Ok(())
}

fn short(sha: &str) -> String {
    sha.chars().take(10).collect()
}
