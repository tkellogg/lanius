//! `lanius estimate set/actual/retro` — the CLI glue for work estimation
//! (docs/handoffs/work-estimation.md, E1–E3). The CLI IS the API: a harness or a
//! package script shells out to it exactly like `lanius block` / `lanius code
//! note`. Every verb reuses an existing primitive — a memory block
//! (`context_store`), an obs event (`trace::write`), the obs projection
//! (`code_projection`/`estimate::compute_actuals`) — so estimation adds NO kernel
//! data model: there is no estimation table and no estimation type here, only
//! reads of the projection and writes of blocks.
//!
//!   * `set`    (E1) — write the `estimate` block (session scope, JSON) and emit
//!                      `obs/estimate/<session>` (the count-from boundary). Latest
//!                      wins (upsert).
//!   * `actual` (E2) — read the `estimate` block + the obs projection, price tokens
//!                      via `pricing.toml`, write the `estimate-vs-actual` block,
//!                      and print the report. A session with no estimate is skipped.
//!   * `retro`  (E3) — append the dated miss to the durable `estimation` block
//!                      (agent scope) so the next estimate reads the prior misses.

use crate::context_blocks::{ContextBlock, Scope};
use crate::context_store;
use crate::db;
use crate::estimate::{
    self, Estimate, Pricing, Report, ESTIMATE_BLOCK, ESTIMATE_VS_ACTUAL_BLOCK, ESTIMATION_BLOCK,
};
use crate::paths::Root;
use crate::profile;
use crate::trace;
use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;

/// Shared addressing for the estimate verbs.
pub struct EstimateOpts {
    pub profile: String,
    pub session: String,
    /// The owner identity (agent noun) the blocks belong to. Defaults to the
    /// profile's agent noun — the same self-attested label `lanius block` uses.
    pub owner: Option<String>,
    /// Override the pricing.toml path. Defaults to the estimation package's
    /// `pricing.toml` under the root's package path, then the kit copy.
    pub pricing: Option<PathBuf>,
}

impl Default for EstimateOpts {
    fn default() -> Self {
        EstimateOpts {
            profile: "default".into(),
            session: "render-preview".into(),
            owner: None,
            pricing: None,
        }
    }
}

fn resolve_owner(root: &Root, opts: &EstimateOpts) -> Result<String> {
    if let Some(o) = &opts.owner {
        return Ok(o.clone());
    }
    let (prof, _) = profile::load(root, &opts.profile)?;
    Ok(prof.agent)
}

/// The session-scope `estimate`/`estimate-vs-actual` block addressed by name, owned
/// by the agent noun — the same shape `load_session_blocks` reads for a coding
/// session, so the agent sees what it committed to in its own context.
fn session_block(name: &str, owner: &str) -> ContextBlock {
    let mut b = ContextBlock::new(name, "", owner);
    b.scope = Scope::Session;
    b.package = Some("estimation".into());
    b
}

/// The durable agent-scope `estimation` block (the learned heuristic).
fn estimation_block(owner: &str) -> ContextBlock {
    let mut b = ContextBlock::new(ESTIMATION_BLOCK, "", owner);
    b.scope = Scope::Agent;
    b.package = Some("estimation".into());
    b
}

/// The once-per-session retro marker (session scope). Its presence means this
/// session already retro'd — the guard that keeps the durable `estimation` block
/// to exactly one entry per session whether the retro is driven by the Stop hook
/// or a cron backstop (both call `retro`).
fn retro_marker_block(owner: &str) -> ContextBlock {
    let mut b = ContextBlock::new("estimation-retro-done", "1", owner);
    b.scope = Scope::Session;
    b.package = Some("estimation".into());
    b
}

/// Resolve the pricing.toml path: explicit `--pricing`, else the estimation
/// package's shipped copy. Returns the first existing candidate, or the primary
/// candidate (load tolerates a missing file → empty map → dollars unavailable).
fn pricing_path(root: &Root, opts: &EstimateOpts) -> PathBuf {
    if let Some(p) = &opts.pricing {
        return p.clone();
    }
    let candidates = [
        root.packages().join("estimation").join("pricing.toml"),
        root.dir.join("kits/core/packages/estimation/pricing.toml"),
    ];
    candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

/// E1 — `lanius estimate set`: record the multi-dimensional estimate. Writes the
/// `estimate` block (latest wins) AND emits `obs/estimate/<session>` carrying the
/// same dims + a timestamp (the count-from boundary).
#[allow(clippy::too_many_arguments)]
pub fn set(
    root: &Root,
    dollars: Option<f64>,
    turns: Option<i64>,
    tokens: Option<i64>,
    wall_clock_ms: Option<i64>,
    opts: &EstimateOpts,
) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let owner = resolve_owner(root, opts)?;
    let at = trace::now_iso();
    let est = Estimate {
        dollars,
        turns,
        tokens,
        wall_clock_ms,
        at: Some(at.clone()),
    };

    // (a) the `estimate` block — last-writer-wins per (session, owner, name).
    let mut block = session_block(ESTIMATE_BLOCK, &owner);
    block.content = est.to_block_content();
    context_store::upsert_block(&conn, &opts.profile, &block, &opts.session, None)?;

    // (b) the `obs/estimate/<session>` event — the boundary on the bus, materialized
    // by the obs projection like any other obs line. trace::write fans out to the
    // bus and (per the recorder's sink rule) appends to trace.jsonl.
    let topic = format!(
        "obs/estimate/{}",
        crate::topic::encode_segment(&opts.session)
    );
    let mut payload = serde_json::to_value(&est).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("session".into(), serde_json::json!(opts.session));
        obj.insert("owner".into(), serde_json::json!(owner));
    }
    trace::write(root, &topic, &trace::Ids::from_env(), payload);

    println!(
        "estimate set for {} (boundary {at}): {}",
        opts.session,
        est.to_block_content()
    );
    Ok(())
}

/// Load this session's recorded estimate, or `None` when none was set (E2/E3 then
/// SKIP the session — never crash).
fn load_estimate(conn: &Connection, owner: &str, session: &str) -> Result<Option<Estimate>> {
    let block = session_block(ESTIMATE_BLOCK, owner);
    match context_store::get_block(conn, &block, session, None)? {
        Some(b) => Ok(Some(Estimate::from_block_content(&b.content)?)),
        None => Ok(None),
    }
}

/// E2 — compute the actuals + variance for a session and write the
/// `estimate-vs-actual` computed block. Returns the report, or `None` when the
/// session has no recorded estimate (skipped). Also runs the obs projection so the
/// actuals reflect the latest trace.
pub fn report(root: &Root, opts: &EstimateOpts) -> Result<Option<Report>> {
    // Materialize the latest obs before reading actuals (best-effort).
    let _ = crate::code_projection::project_trace(root);
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let owner = resolve_owner(root, opts)?;
    let Some(est) = load_estimate(&conn, &owner, &opts.session)? else {
        return Ok(None); // no estimate → skip, no crash
    };
    let pricing = Pricing::load(&pricing_path(root, opts))?;
    let actuals = estimate::compute_actuals(&conn, &opts.session, est.at.as_deref(), &pricing)?;
    let report = Report::build(&opts.session, &est, &actuals);

    // Write the computed `estimate-vs-actual` block (session scope).
    let mut block = session_block(ESTIMATE_VS_ACTUAL_BLOCK, &owner);
    block.content = report.to_block_content();
    context_store::upsert_block(&conn, &opts.profile, &block, &opts.session, None)?;

    Ok(Some(report))
}

/// `lanius estimate actual`: E2 as a command — print the report (or a skip note).
pub fn actual(root: &Root, opts: &EstimateOpts) -> Result<()> {
    match report(root, opts)? {
        Some(r) => {
            if r.dollars_unavailable {
                eprintln!(
                    "note: dollars unavailable for {} (no priced token usage; see pricing.toml)",
                    opts.session
                );
            }
            println!("{}", r.to_block_content());
        }
        None => println!("no estimate recorded for {} (skipped)", opts.session),
    }
    Ok(())
}

/// `lanius estimate actual --json`: E2 as a machine-readable command — print the
/// `Report` as JSON, or `null` when the session has no recorded estimate (so the
/// web /api/estimate/{session} route can distinguish "no estimate" from an error
/// and simply omit the estimate group). Never crashes on a missing estimate.
pub fn actual_json(root: &Root, opts: &EstimateOpts) -> Result<()> {
    match report(root, opts)? {
        Some(r) => println!("{}", serde_json::to_string_pretty(&r)?),
        None => println!("null"),
    }
    Ok(())
}

/// E3 — the retro: compute the variance and append a dated miss to the durable
/// `estimation` block (agent scope), the default-that-evolves loop. A session with
/// no estimate is skipped. Returns the appended note (for the caller/hook to log),
/// or `None` when skipped. Idempotent-ish: the block is append-only by design, so a
/// caller that retros twice records two entries — callers (the Stop hook) guard with
/// a once-per-session marker.
pub fn retro(root: &Root, opts: &EstimateOpts) -> Result<Option<String>> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let owner = resolve_owner(root, opts)?;

    // Once-per-session guard: a marker block means this session already retro'd
    // (a run emits several Stop events; a cron backstop may also fire). Skip so the
    // durable block gains exactly one entry per session.
    let marker = retro_marker_block(&owner);
    if context_store::get_block(&conn, &marker, &opts.session, None)?.is_some() {
        return Ok(None);
    }

    let Some(report) = report(root, opts)? else {
        return Ok(None);
    };
    let date = trace::now_iso();
    let date = date.split('T').next().unwrap_or(&date).to_string();
    let note = report.retro_note(&date);

    // Append to the durable `estimation` block (create if absent).
    let mut block = estimation_block(&owner);
    let existing = context_store::get_block(&conn, &block, &opts.session, None)?;
    block.content = match existing {
        Some(b) if !b.content.is_empty() => format!("{}\n{note}", b.content),
        _ => note.clone(),
    };
    context_store::upsert_block(&conn, &opts.profile, &block, &opts.session, None)?;
    // Mark this session retro'd so a later Stop/cron does not double-record.
    context_store::upsert_block(&conn, &opts.profile, &marker, &opts.session, None)?;
    Ok(Some(note))
}

/// `lanius estimate retro`: E3 as a command — print the appended note (or a skip).
pub fn retro_cmd(root: &Root, opts: &EstimateOpts) -> Result<()> {
    match retro(root, opts)? {
        Some(note) => println!("estimation block updated: {note}"),
        None => println!("no estimate recorded for {} (skipped)", opts.session),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::io::Write;

    fn temp_root(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!(
            "lanius-estimate-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        // A default profile so resolve_owner has an agent noun.
        std::fs::create_dir_all(dir.join("profiles/default")).unwrap();
        std::fs::write(
            dir.join("profiles/default/profile.toml"),
            "agent = \"claude-code\"\nowner = \"owner\"\n",
        )
        .unwrap();
        Root { dir }
    }

    /// Rewrite the recorded estimate's boundary `at` to a fixed timestamp, so the
    /// actuals computation counts the test's seeded (fixed-date) events. In a real
    /// run the boundary is "now" and the events follow it; the seeded fixtures use
    /// hard-coded dates, so the test pins the boundary to just before them.
    fn pin_boundary(root: &Root, owner: &str, session: &str, at: &str) {
        let conn = db::open(root).unwrap();
        let mut est = load_estimate(&conn, owner, session).unwrap().unwrap();
        est.at = Some(at.into());
        let mut block = session_block(ESTIMATE_BLOCK, owner);
        block.content = est.to_block_content();
        context_store::upsert_block(&conn, "default", &block, session, None).unwrap();
    }

    fn opts(session: &str) -> EstimateOpts {
        EstimateOpts {
            profile: "default".into(),
            session: session.into(),
            owner: Some("claude-code".into()),
            pricing: None,
        }
    }

    /// Seed an obs line into trace.jsonl exactly as the recorder would.
    fn append_trace(root: &Root, kind: &str, ts: &str, payload: Value) {
        let mut p = payload;
        if let Some(o) = p.as_object_mut() {
            o.insert("ts".into(), json!(ts));
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.trace_file())
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({ "ts": ts, "kind": kind, "payload": p, "sender": "test" })
        )
        .unwrap();
    }

    // ── E1 — estimate persists as a block + the obs event; latest wins ──────────
    #[test]
    fn e1_estimate_persists_as_block_and_obs_event_latest_wins() {
        let root = temp_root("e1");
        set(
            &root,
            Some(0.40),
            Some(8),
            Some(1000),
            Some(60_000),
            &opts("code-e1"),
        )
        .unwrap();

        // (a) the `estimate` block holds the JSON.
        let conn = db::open(&root).unwrap();
        let est = load_estimate(&conn, "claude-code", "code-e1")
            .unwrap()
            .unwrap();
        assert_eq!(est.dollars, Some(0.40));
        assert_eq!(est.turns, Some(8));
        assert!(est.at.is_some(), "boundary timestamp recorded");

        // (b) an obs/estimate/<session> event reached the trace with the dims.
        let trace = std::fs::read_to_string(root.trace_file()).unwrap();
        assert!(
            trace.contains("obs/estimate/code-e1"),
            "estimate event on the bus: {trace}"
        );

        // Latest wins: a second set updates the block (not a second row).
        set(&root, Some(0.99), Some(20), None, None, &opts("code-e1")).unwrap();
        let est2 = load_estimate(&conn, "claude-code", "code-e1")
            .unwrap()
            .unwrap();
        assert_eq!(est2.dollars, Some(0.99));
        assert_eq!(est2.turns, Some(20));
        // The session-scope estimate block is unique per (session, owner, name).
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM context_blocks WHERE name='estimate' AND owner='claude-code'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "latest-wins keeps one estimate row");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    // ── E2 — actuals from a SEEDED projection; variance; pricing; skip ──────────
    #[test]
    fn e2_actuals_variance_pricing_and_skip() {
        let root = temp_root("e2");
        // Ship a package-local pricing.toml under the root's package path.
        let pkg = root.packages().join("estimation");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("pricing.toml"),
            "[models.\"gpt-5\"]\ninput = 0.000001\noutput = 0.000002\n",
        )
        .unwrap();

        // Seed an obs projection: a session/start (model gpt-5), three tool calls,
        // two user turns, token usage, and a stop — all AFTER the estimate boundary.
        let s = "code-e2";
        let t = |n: u32| format!("2026-06-23T00:0{n}:00.000Z");
        append_trace(
            &root,
            &format!("obs/agent/claude-code/{s}/session/start"),
            &t(0),
            json!({ "tool": "claude", "model": "gpt-5" }),
        );
        // The estimate boundary is t(1): everything from here counts.
        set(
            &root,
            Some(0.40),
            Some(2),
            Some(1000),
            Some(60_000),
            &opts(s),
        )
        .unwrap();
        pin_boundary(&root, "claude-code", s, &t(1));
        for n in 2..=4 {
            append_trace(
                &root,
                &format!("obs/agent/claude-code/{s}/tool/edit/call"),
                &t(n),
                json!({ "tool": "edit" }),
            );
        }
        append_trace(
            &root,
            &format!("obs/agent/claude-code/{s}/user/message"),
            &t(5),
            json!({ "prompt": "again" }),
        );
        append_trace(
            &root,
            &format!("obs/agent/claude-code/{s}/user/message"),
            &t(6),
            json!({ "prompt": "more" }),
        );
        append_trace(
            &root,
            &format!("obs/agent/claude-code/{s}/session/idle"),
            &t(7),
            json!({ "usage": { "input_tokens": 100, "output_tokens": 50 } }),
        );
        append_trace(
            &root,
            &format!("obs/agent/claude-code/{s}/session/stop"),
            &t(8),
            json!({ "exit_code": 0 }),
        );

        let r = report(&root, &opts(s)).unwrap().expect("estimate present");
        // Turns: two user/message events. Tool calls: three.
        assert_eq!(r.turns.actual, Some(2.0));
        assert_eq!(r.tool_calls.actual, Some(3.0));
        // Turn variance vs the estimate of 2: 2 - 2 = 0.
        assert_eq!(r.turns.estimate, Some(2.0));
        assert_eq!(r.turns.delta, Some(0.0));
        // Tokens folded: 100 + 50 = 150.
        assert_eq!(r.tokens.actual, Some(150.0));
        // Dollars via pricing: 100×1e-6 + 50×2e-6 = 0.0002.
        let d = r.dollars.actual.unwrap();
        assert!((d - 0.0002).abs() < 1e-9, "dollars {d}");
        assert!(!r.dollars_unavailable);
        // Dollar variance vs $0.40 estimate.
        assert!((r.dollars.delta.unwrap() - (0.0002 - 0.40)).abs() < 1e-9);

        // The computed `estimate-vs-actual` block was written.
        let conn = db::open(&root).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM context_blocks WHERE name='estimate-vs-actual'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);

        // A session with NO estimate is skipped (None, no crash).
        assert!(report(&root, &opts("code-no-estimate")).unwrap().is_none());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn e2_dollars_unavailable_when_unpriced() {
        let root = temp_root("e2-noprice");
        // No pricing.toml shipped → dollars unavailable, but the rest still reports.
        let s = "code-noprice";
        append_trace(
            &root,
            &format!("obs/agent/claude-code/{s}/session/start"),
            "2026-06-23T00:00:00.000Z",
            json!({ "tool": "claude", "model": "mystery-model" }),
        );
        set(&root, Some(0.40), Some(2), None, None, &opts(s)).unwrap();
        append_trace(
            &root,
            &format!("obs/agent/claude-code/{s}/session/idle"),
            "2026-06-23T00:05:00.000Z",
            json!({ "usage": { "input_tokens": 100, "output_tokens": 50 } }),
        );
        let r = report(&root, &opts(s)).unwrap().unwrap();
        assert!(r.dollars_unavailable, "no pricing → dollars unavailable");
        assert!(r.dollars.actual.is_none());
        // Tokens are still reported (usage was present).
        assert_eq!(r.tokens.actual, Some(150.0));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    // ── E3 — durable estimation block gains a dated miss, survives sessions ─────
    #[test]
    fn e3_retro_appends_durable_miss_across_sessions() {
        let root = temp_root("e3");
        let pkg = root.packages().join("estimation");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("pricing.toml"),
            "[models.\"gpt-5\"]\ninput = 0.000001\noutput = 0.000002\n",
        )
        .unwrap();

        // Session 1: estimate + actuals, then retro.
        let s1 = "code-e3a";
        append_trace(
            &root,
            &format!("obs/agent/claude-code/{s1}/session/start"),
            "2026-06-23T00:00:00.000Z",
            json!({ "tool": "claude", "model": "gpt-5" }),
        );
        set(&root, Some(0.40), Some(8), None, None, &opts(s1)).unwrap();
        append_trace(
            &root,
            &format!("obs/agent/claude-code/{s1}/session/idle"),
            "2026-06-23T00:05:00.000Z",
            json!({ "usage": { "input_tokens": 300, "output_tokens": 200 } }),
        );
        let note1 = retro(&root, &opts(s1)).unwrap().expect("retro recorded");
        assert!(note1.contains("estimated $0.40"), "{note1}");
        // Once-per-session: a second retro on the SAME session is a no-op (the Stop
        // hook fires several times; a cron backstop may also run).
        assert!(
            retro(&root, &opts(s1)).unwrap().is_none(),
            "retro is idempotent per session"
        );

        // The durable agent-scope `estimation` block now carries the dated miss.
        let conn = db::open(&root).unwrap();
        let block = estimation_block("claude-code");
        let stored = context_store::get_block(&conn, &block, s1, None)
            .unwrap()
            .unwrap();
        // Dated with TODAY's date (the retro stamps wall-clock now, not the fixture
        // dates) — assert the YYYY-MM-DD shape is present rather than a fixed date.
        let today = trace::now_iso();
        let today = today.split('T').next().unwrap();
        assert!(
            stored.content.contains(today),
            "dated {today}: {}",
            stored.content
        );
        assert!(stored.content.contains("estimated $0.40"));

        // Session 2 (a DIFFERENT session) retros into the SAME durable block — it
        // survives across sessions and accumulates.
        let s2 = "code-e3b";
        append_trace(
            &root,
            &format!("obs/agent/claude-code/{s2}/session/start"),
            "2026-06-24T00:00:00.000Z",
            json!({ "tool": "claude", "model": "gpt-5" }),
        );
        set(&root, Some(1.00), Some(20), None, None, &opts(s2)).unwrap();
        append_trace(
            &root,
            &format!("obs/agent/claude-code/{s2}/session/idle"),
            "2026-06-24T00:05:00.000Z",
            json!({ "usage": { "input_tokens": 10, "output_tokens": 10 } }),
        );
        retro(&root, &opts(s2)).unwrap().unwrap();

        // The agent-scope block (no session binding) is visible from any session and
        // now holds BOTH misses.
        let stored2 = context_store::get_block(&conn, &block, s2, None)
            .unwrap()
            .unwrap();
        let lines: Vec<&str> = stored2.content.lines().collect();
        assert_eq!(lines.len(), 2, "two dated misses accumulate: {:?}", lines);
        assert!(stored2.content.contains("estimated $0.40"));
        assert!(stored2.content.contains("estimated $1.00"));

        // A session with no estimate retros to None (skip, no crash).
        assert!(retro(&root, &opts("code-none")).unwrap().is_none());
        std::fs::remove_dir_all(&root.dir).ok();
    }
}
