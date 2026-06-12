use crate::context;
use crate::db;
use crate::events::{self, EmitOpts};
use crate::hooks;
use crate::paths::Root;
use crate::profile;
use crate::render;
use crate::sandbox;
use crate::trace;
use anyhow::{anyhow, bail, Context, Result};
use genai::chat::{ChatMessage, ChatRequest, ContentPart, MessageContent, Tool, ToolCall, ToolResponse};
use genai::Client;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Read as _;

pub struct ExecOpts {
    pub session: Option<String>,
    pub profile: String,
    pub prompt: Option<String>,
    pub resume: Option<String>,
    /// The dispatching event for the context document's `event` field
    /// ({topic, payload, correlation_id}); None for CLI-direct runs.
    pub event: Option<Value>,
}

/// What a tool invocation produced. Model-caused errors (bad args, unknown
/// tool) are Output too — they go back to the model as results so it can
/// self-correct; aborting the exec would strand the stored tool-call message.
enum ToolOutcome {
    Output(String),
    /// ask_human under the daemon: bookkeeping done, caller finishes the
    /// batch with synthetic results and exits 75.
    Suspend,
}

/// `elanus exec` — run an agent turn. Chat is exec with a session ID.
/// The tool loop is hand-rolled on purpose: termination policy, signal
/// preemption, budget enforcement, and trace capture live here and are owned.
pub fn run(root: &Root, opts: ExecOpts) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(root, opts))
}

async fn run_async(root: &Root, opts: ExecOpts) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let (prof, _pdir) = profile::load(root, &opts.profile)?;
    let session = opts
        .session
        .unwrap_or_else(|| format!("s-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    let mut ids = trace::Ids::from_env();
    ids.session_id = Some(session.clone());
    let event_id = ids.event_id;
    let in_handler = event_id.is_some();

    let prompt = match opts.prompt {
        Some(p) if p == "-" => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Some(s.trim().to_string())
        }
        other => other,
    };

    if let Some(ans) = &opts.resume {
        // Continuation after suspend: the parked tool call gets its response.
        let key = pending_ask_key(&session);
        let pend = db::kv_get(&conn, &key)?
            .ok_or_else(|| anyhow!("no pending ask_human recorded for session {session}"))?;
        let pend: Value = serde_json::from_str(&pend)?;
        store_msg(
            &conn,
            &session,
            event_id,
            &json!({
                "role": "tool",
                "tool_call_id": pend["call_id"],
                "name": "ask_human",
                "content": json!({ "answer": ans }).to_string(),
            }),
        )?;
        db::kv_del(&conn, &key)?;
        // Close the loop in the event log: a CLI --resume is an answer too,
        // otherwise the ask sits in the inbox forever. The daemon flow already
        // has an answer event (it triggered the resume) — dedupe on correlation.
        if let Some(corr) = pend["correlation"].as_str() {
            let answer_mb = crate::topic::agent_mailbox(&prof.agent);
            let already: i64 = conn.query_row(
                "SELECT COUNT(*) FROM events WHERE type=?1 AND correlation_id=?2",
                [answer_mb.as_str(), corr],
                |r| r.get(0),
            )?;
            if already == 0 {
                events::emit(
                    root,
                    &conn,
                    EmitOpts {
                        payload: Some(json!({ "answer": ans, "via": "exec-resume" })),
                        correlation: Some(corr.to_string()),
                        cause: pend["ask_id"].as_i64(),
                        ..EmitOpts::new(&answer_mb)
                    },
                )?;
            }
        }
    }

    // Crash repair BEFORE appending new input: any assistant tool call with no
    // recorded result (process died mid-tool, or an abandoned suspend) gets a
    // synthetic "interrupted" result so the replayed transcript stays valid.
    let repaired = repair_transcript(&conn, &session)?;
    if repaired > 0 {
        eprintln!("[exec] repaired {repaired} interrupted tool call(s) in session {session}");
    }

    if opts.resume.is_none() {
        if let Some(p) = &prompt {
            store_msg(&conn, &session, event_id, &json!({ "role": "user", "text": p }))?;
        } else {
            bail!("nothing to do: provide a prompt, '-' for stdin, or --resume");
        }
    }

    // Context pipeline (docs/context.md): the system SEED is computed once
    // per run (blocks + providers + skills inventory — pre-pipeline
    // semantics, providers don't re-spawn per call); the chain runs before
    // every LLM call over the freshly re-read transcript.
    let system_seed = render::render_parts(root, &conn, &opts.profile, &session)?;
    let stages = context::chain(root, &conn, &prof)?;
    let event_doc = opts.event.clone().unwrap_or(Value::Null);
    // The cage is built once per exec from the profile's [sandbox] grant;
    // every shell tool call spawns inside it and gets boundary-diffed.
    let cage = sandbox::Cage::from_profile(root, &prof.sandbox);
    let client = build_client(&prof);
    let model = prof.model.model.clone();
    // MCP servers (border protocol, src/mcp.rs): approved third-party tool
    // servers spawn inside the agent's cage for the run; their tools join
    // the array as <server>__<tool>. Failures degrade loudly, never fatal.
    let mcp_pool = crate::mcp::Pool::load(root, &conn, &prof, &cage);
    let mut tools = tool_defs();
    tools.extend(mcp_pool.tool_defs());
    let root_type = match event_id {
        Some(id) => db::root_type(&conn, id).unwrap_or_else(|_| "cli".into()),
        None => "cli".into(),
    };
    let mut signal_watermark: i64 = conn.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM events WHERE type LIKE 'signal/%'",
        [],
        |r| r.get(0),
    )?;
    // Events emitted by this exec; excluded from signal preemption so an agent
    // emitting signal/pain doesn't get its own scream echoed back (feedback loop).
    let mut self_emitted: HashSet<i64> = HashSet::new();

    let mut turns = 0u32;
    loop {
        turns += 1;
        if turns > prof.model.max_turns {
            events::emit(
                root,
                &conn,
                EmitOpts {
                    payload: Some(json!({
                        "reason": "max_turns reached",
                        "session": session, "turns": turns,
                    })),
                    cause: event_id,
                    ..EmitOpts::new("signal/pain")
                },
            )?;
            bail!("max_turns ({}) reached for session {session}", prof.model.max_turns);
        }
        check_token_budget(root, &conn, &root_type, event_id)?;

        let doc = context::assemble(
            root,
            &system_seed,
            transcript_rows(&conn, &session)?,
            event_doc.clone(),
            context::Meta {
                profile: opts.profile.clone(),
                agent: prof.agent.clone(),
                session: session.clone(),
                turn: turns,
                model: model.clone(),
            },
            &stages,
            &ids,
        )?;
        let chat_req = build_request(&doc, &tools)?;
        trace::write(root, &obs(&prof.agent, &session, "llm/request"), &ids, json!({ "model": model, "turn": turns }));
        let res = client
            .exec_chat(model.as_str(), chat_req, None)
            .await
            .with_context(|| format!("llm call failed (model {model})"))?;
        let tokens_in = res.usage.prompt_tokens.unwrap_or(0);
        let tokens_out = res.usage.completion_tokens.unwrap_or(0);
        conn.execute(
            "INSERT INTO llm_usage(event_id, root_type, model, input_tokens, output_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![event_id, root_type, model, tokens_in, tokens_out],
        )?;
        let text = res.first_text().map(|s| s.to_string());
        let tool_calls: Vec<ToolCall> = res.into_tool_calls();
        trace::write(
            root,
            &obs(&prof.agent, &session, "llm/response"),
            &ids,
            json!({
                "model": model,
                "input_tokens": tokens_in,
                "output_tokens": tokens_out,
                "tool_calls": tool_calls.iter().map(|t| t.fn_name.clone()).collect::<Vec<_>>(),
                "text": text.as_deref().map(|t| trace::clip(t, 2000)),
            }),
        );

        let mut amsg = json!({ "role": "assistant" });
        if let Some(t) = &text {
            amsg["text"] = json!(t);
        }
        if !tool_calls.is_empty() {
            amsg["tool_calls"] = json!(tool_calls
                .iter()
                .map(|c| json!({
                    "call_id": c.call_id, "fn_name": c.fn_name, "fn_arguments": c.fn_arguments
                }))
                .collect::<Vec<_>>());
        }
        store_msg(&conn, &session, event_id, &amsg)?;

        if tool_calls.is_empty() {
            let out = text.unwrap_or_default();
            println!("{out}");
            // The mailbox model's last leg: a correlated, dispatched run is a
            // conversation turn, and the reply is MAIL TO THE HUMAN — not just
            // transcript + captured stdout. Same correlation = same thread
            // (the TUI/CLI that composed the work sees the reply arrive).
            // Uncorrelated or CLI-direct runs stay quiet: background work's
            // results live in the transcript, not the human's inbox.
            if in_handler && !out.is_empty() {
                if let Some(corr) = ids.correlation_id.clone() {
                    events::emit(
                        root,
                        &conn,
                        EmitOpts {
                            payload: Some(json!({ "text": out })),
                            correlation: Some(corr),
                            cause: event_id,
                            ..EmitOpts::new(&crate::topic::human_mailbox(&prof.owner))
                        },
                    )?;
                }
            }
            return Ok(());
        }

        let mut suspended_at: Option<usize> = None;
        for (i, call) in tool_calls.iter().enumerate() {
            // Hook plane, pre_tool_call: may rewrite args or veto. The
            // transcript keeps the model's ORIGINAL call (it must replay as
            // sent); rewritten args live in execution and the trace. A veto
            // becomes an ordinary error result so the model can adapt.
            // Chain order: exec hooks first (local, stateless, no round
            // trip), then resident hooks on the exec-rewritten subject; the
            // first deny anywhere short-circuits — a denied call never pays
            // the resident round trip. The resident consult is zero-cost
            // when nothing is registered (kv gate, src/resident.rs).
            let pre = hooks::run_chain(
                root,
                &conn,
                "pre_tool_call",
                &call.fn_name,
                json!({ "point": "pre_tool_call", "session": session,
                        "tool": call.fn_name, "args": call.fn_arguments }),
                &ids,
            )?;
            let pre = if pre.allow {
                crate::resident::consult(root, &conn, "pre_tool_call", &call.fn_name, pre.subject, &ids)
            } else {
                pre
            };
            let eff = ToolCall {
                call_id: call.call_id.clone(),
                fn_name: call.fn_name.clone(),
                fn_arguments: pre
                    .subject
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| call.fn_arguments.clone()),
                thought_signatures: None,
            };
            // The (effective) tool call goes to the trace BEFORE execution: a
            // crash mid-tool must be visible as a call with no result.
            trace::write(
                root,
                &obs_tool(&prof.agent, &session, &eff.fn_name, "call"),
                &ids,
                json!({ "call_id": eff.call_id, "name": eff.fn_name, "args": eff.fn_arguments }),
            );
            if !pre.allow {
                let result = json!({
                    "error": format!("blocked by hook {}", pre.denied_by.as_deref().unwrap_or("?")),
                    "reason": pre.reason,
                })
                .to_string();
                trace::write(
                    root,
                    &obs_tool(&prof.agent, &session, &eff.fn_name, "result"),
                    &ids,
                    json!({ "call_id": eff.call_id, "name": eff.fn_name, "denied": true, "result": trace::clip(&result, 2000) }),
                );
                store_msg(
                    &conn,
                    &session,
                    event_id,
                    &json!({
                        "role": "tool",
                        "tool_call_id": eff.call_id,
                        "name": eff.fn_name,
                        "content": result,
                    }),
                )?;
                continue;
            }
            match run_tool(root, &conn, &cage, &prof, &session, event_id, in_handler, &eff, &mut self_emitted, &mcp_pool) {
                ToolOutcome::Output(result) => {
                    // Hook plane, post_tool_call: may scrub/rewrite the result
                    // or veto it (the model then sees the denial, not the data).
                    // Same order as pre: exec chain, then resident chain.
                    let post = hooks::run_chain(
                        root,
                        &conn,
                        "post_tool_call",
                        &eff.fn_name,
                        json!({ "point": "post_tool_call", "session": session,
                                "tool": eff.fn_name, "args": eff.fn_arguments,
                                "result": result }),
                        &ids,
                    )?;
                    let post = if post.allow {
                        crate::resident::consult(root, &conn, "post_tool_call", &eff.fn_name, post.subject, &ids)
                    } else {
                        post
                    };
                    let result = if !post.allow {
                        json!({
                            "error": format!("result blocked by hook {}", post.denied_by.as_deref().unwrap_or("?")),
                            "reason": post.reason,
                        })
                        .to_string()
                    } else {
                        match post.subject.get("result") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) if !v.is_null() => v.to_string(),
                            _ => result,
                        }
                    };
                    trace::write(
                        root,
                        &obs_tool(&prof.agent, &session, &eff.fn_name, "result"),
                        &ids,
                        json!({ "call_id": eff.call_id, "name": eff.fn_name, "result": trace::clip(&result, 2000) }),
                    );
                    store_msg(
                        &conn,
                        &session,
                        event_id,
                        &json!({
                            "role": "tool",
                            "tool_call_id": eff.call_id,
                            "name": eff.fn_name,
                            "content": result,
                        }),
                    )?;
                }
                ToolOutcome::Suspend => {
                    suspended_at = Some(i);
                    break;
                }
            }
        }
        if let Some(i) = suspended_at {
            // Sibling calls after the parked one get synthetic results NOW so
            // the transcript replays cleanly on resume; only the ask itself
            // stays open (its response arrives with the answer).
            let interrupted =
                json!({ "error": "interrupted: run suspended while waiting on the human" }).to_string();
            for call in &tool_calls[i + 1..] {
                trace::write(
                    root,
                    &obs_tool(&prof.agent, &session, &call.fn_name, "result"),
                    &ids,
                    json!({ "call_id": call.call_id, "name": call.fn_name, "interrupted": true }),
                );
                store_msg(
                    &conn,
                    &session,
                    event_id,
                    &json!({
                        "role": "tool",
                        "tool_call_id": call.call_id,
                        "name": call.fn_name,
                        "content": interrupted,
                    }),
                )?;
            }
            // Checkpoint-and-exit, never block: the transcript in sqlite is
            // the process state; 75 tells the dispatcher to park this chain.
            std::process::exit(75);
        }

        // Algedonic preemption: between tool batches, new signal/# events
        // (not our own) interrupt the loop as injected context.
        let sigs: Vec<(i64, String, Option<String>)> = {
            let mut stmt = conn.prepare(
                "SELECT id, type, payload FROM events WHERE type LIKE 'signal/%' AND id > ?1 ORDER BY id",
            )?;
            let r = stmt
                .query_map([signal_watermark], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            r
        };
        signal_watermark = sigs.iter().map(|s| s.0).max().unwrap_or(signal_watermark);
        let sigs: Vec<_> = sigs.into_iter().filter(|(id, _, _)| !self_emitted.contains(id)).collect();
        if !sigs.is_empty() {
            let note = sigs
                .iter()
                .map(|(id, t, p)| format!("[signal #{id}] {t} {}", p.as_deref().unwrap_or("{}")))
                .collect::<Vec<_>>()
                .join("\n");
            trace::write(root, &obs(&prof.agent, &session, "signal/injected"), &ids, json!({ "injected": sigs.len() }));
            store_msg(
                &conn,
                &session,
                event_id,
                &json!({ "role": "user", "text": format!("(harness) signals arrived while you were working:\n{note}") }),
            )?;
        }
    }
}

fn pending_ask_key(session: &str) -> String {
    format!("session:{session}:pending_ask")
}

/// Session-scoped observation topic: obs/agent/<agent>/<session>/<rest>.
fn obs(agent: &str, session: &str, rest: &str) -> String {
    format!(
        "obs/agent/{}/{}/{rest}",
        crate::topic::encode_segment(agent),
        crate::topic::encode_segment(session)
    )
}

/// Tool-scoped observation topic: obs/agent/<agent>/<session>/tool/<name>/<leaf>.
fn obs_tool(agent: &str, session: &str, tool: &str, leaf: &str) -> String {
    obs(agent, session, &format!("tool/{}/{leaf}", crate::topic::encode_segment(tool)))
}

// ── Leases: &mut on subtrees, the kernel as borrow checker ─────────────────
// (docs/sandbox.md). Lease lifetime = holder lifetime: the dispatcher
// releases leases of finished dispatches and dead pids; a standalone exec
// releases its own on clean exit. There is no unlock call to forget.

/// Who holds leases acquired by this process: the enclosing dispatch when
/// there is one (survives suspend/resume), else this pid.
fn lease_holder() -> (Option<i64>, i64) {
    let dispatch = std::env::var("HARNESS_DISPATCH_ID").ok().and_then(|v| v.parse().ok());
    (dispatch, std::process::id() as i64)
}

fn same_holder(dispatch: Option<i64>, pid: i64, row_dispatch: Option<i64>, row_pid: Option<i64>) -> bool {
    match (dispatch, row_dispatch) {
        (Some(a), Some(b)) => a == b,
        _ => row_pid == Some(pid),
    }
}

fn acquire_lease(
    root: &Root,
    conn: &Connection,
    cage: &sandbox::Cage,
    agent: &str,
    session: &str,
    path: &str,
) -> anyhow::Result<String> {
    let p = std::path::Path::new(path);
    let p = if p.is_absolute() { p.to_path_buf() } else { root.dir.join(p) };
    let canon = p
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("{} is not an existing directory: {e}", p.display()))?;
    if !canon.is_dir() {
        anyhow::bail!("{} is not a directory", canon.display());
    }
    // lease ⊆ grant: a decidable, boring prefix check (docs/sandbox.md).
    if !cage.write_roots.iter().any(|r| canon.starts_with(r)) {
        anyhow::bail!(
            "{} is outside the granted write roots {:?}",
            canon.display(),
            cage.write_roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>()
        );
    }
    let (dispatch, pid) = lease_holder();
    // The borrow check must be atomic against other leasers: the overlap
    // scan and the insert run inside one IMMEDIATE transaction so two
    // concurrent processes cannot both pass the scan and then both insert
    // overlapping leases (the check-then-act race). BEGIN IMMEDIATE takes
    // the write lock up front; db::open set a 5s busy_timeout, so the loser
    // waits rather than erroring. The kernel really is the borrow checker.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let outcome = (|| -> anyhow::Result<()> {
        let active: Vec<(String, Option<i64>, Option<i64>)> = {
            let mut stmt =
                conn.prepare("SELECT path, dispatch_id, pid FROM leases WHERE released_at IS NULL")?;
            let r = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            r
        };
        for (held_path, row_dispatch, row_pid) in &active {
            if same_holder(dispatch, pid, *row_dispatch, *row_pid) {
                continue;
            }
            let held = std::path::Path::new(held_path);
            if canon.starts_with(held) || held.starts_with(&canon) {
                anyhow::bail!(
                    "conflicts with an active lease on {held_path} held by another run; \
                     wait for it to finish or lease a disjoint subtree"
                );
            }
        }
        conn.execute(
            "INSERT INTO leases(path, session_id, dispatch_id, pid) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![canon.display().to_string(), session, dispatch, pid],
        )?;
        Ok(())
    })();
    match &outcome {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
    outcome?;
    trace::write(
        root,
        &obs(agent, session, "lease/acquire"),
        &trace::Ids { session_id: Some(session.into()), ..Default::default() },
        json!({ "path": canon.display().to_string(), "dispatch_id": dispatch, "pid": pid }),
    );
    Ok(canon.display().to_string())
}

/// Active leases held by this process's holder identity. Returns Err on a
/// query failure rather than an empty Vec, so the caller does not mistake
/// "couldn't read the table" for "holds nothing" and silently drop cage
/// narrowing (a lease-exclusivity break).
fn held_leases(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let (dispatch, pid) = lease_holder();
    let mut stmt =
        conn.prepare("SELECT path, dispatch_id, pid FROM leases WHERE released_at IS NULL")?;
    let rows: Vec<(String, Option<i64>, Option<i64>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, d, p)| same_holder(dispatch, pid, *d, *p))
        .map(|(path, _, _)| path)
        .collect())
}

/// When leases are held, the spawn cage is the lease write set (plus the
/// harness root — the kernel must not cage itself out of its own ledger).
/// Ok(None) = no leases held, use the base grant cage. Err = the lease
/// table could not be read; the caller fails the tool closed rather than
/// run with the un-narrowed grant.
fn narrowed_cage(root: &Root, conn: &Connection, base: &sandbox::Cage) -> anyhow::Result<Option<sandbox::Cage>> {
    let held = held_leases(conn)?;
    if held.is_empty() {
        return Ok(None);
    }
    let mut roots = vec![root.dir.clone()];
    roots.extend(held.into_iter().map(std::path::PathBuf::from));
    Ok(Some(sandbox::Cage::from_roots(roots, base.exclude.clone(), true)))
}

/// Resolve the profile's `[sandbox] workdir`: tilde-expanded, must be an
/// absolute path to an existing directory. None = run in the harness root,
/// as ever. This is *location*, not authority — writes still flow through
/// the whole-agent grant + leases; the cage is unchanged by it.
fn resolve_workdir(cfg: &profile::SandboxCfg) -> anyhow::Result<Option<std::path::PathBuf>> {
    let Some(w) = &cfg.workdir else { return Ok(None) };
    let expanded = expand_tilde(w);
    let p = std::path::PathBuf::from(&expanded);
    if !p.is_absolute() {
        anyhow::bail!("sandbox.workdir must be an absolute path, got {w:?}");
    }
    if !p.is_dir() {
        anyhow::bail!(
            "sandbox.workdir {} does not exist (or is not a directory); \
             create it or fix the profile — refusing to fall back to the harness root",
            p.display()
        );
    }
    Ok(Some(p))
}

fn expand_tilde(s: &str) -> String {
    if s == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return home;
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{}/{rest}", home.trim_end_matches('/'));
        }
    }
    s.to_string()
}

/// Standalone (non-dispatched) exec: release own leases on clean exit.
pub fn release_own_leases(conn: &Connection) {
    let (dispatch, pid) = lease_holder();
    if dispatch.is_some() {
        return; // the dispatcher owns that lifecycle
    }
    let _ = conn.execute(
        "UPDATE leases SET released_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE released_at IS NULL AND dispatch_id IS NULL AND pid = ?1",
        [pid],
    );
}

/// Default client unless an endpoint/auth override is in play — then a
/// ServiceTargetResolver rewrites the target. ANTHROPIC_BASE_URL (env) only
/// applies when the model resolved to the Anthropic adapter, mirroring the
/// Anthropic SDK; a profile's explicit base_url applies unconditionally.
fn build_client(prof: &profile::Profile) -> Client {
    use genai::adapter::AdapterKind;
    use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
    use genai::ServiceTarget;

    let profile_url = prof.model.base_url.clone();
    let env_url = std::env::var("ANTHROPIC_BASE_URL").ok().filter(|s| !s.is_empty());
    let api_key_env = prof.model.api_key_env.clone();
    if profile_url.is_none() && env_url.is_none() && api_key_env.is_none() {
        return Client::default();
    }
    let resolver = ServiceTargetResolver::from_resolver_fn(
        move |mut target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
            let adapter = target.model.adapter_kind;
            let url = profile_url
                .clone()
                .or_else(|| (adapter == AdapterKind::Anthropic).then(|| env_url.clone()).flatten());
            if let Some(url) = url {
                target.endpoint = Endpoint::from_owned(normalize_base_url(&url, adapter));
            }
            if let Some(envk) = &api_key_env {
                target.auth = AuthData::from_env(envk.clone());
            }
            Ok(target)
        },
    );
    Client::builder().with_service_target_resolver(resolver).build()
}

/// genai appends e.g. "messages" directly to the endpoint, while SDK-style
/// base URLs (ANTHROPIC_BASE_URL) expect "/v1/messages" appended. Normalize:
/// trailing slash always; for the Anthropic adapter, a missing /v1/ is added.
fn normalize_base_url(url: &str, adapter: genai::adapter::AdapterKind) -> String {
    let trimmed = url.trim_end_matches('/');
    if adapter == genai::adapter::AdapterKind::Anthropic && !trimmed.ends_with("/v1") {
        format!("{trimmed}/v1/")
    } else {
        format!("{trimmed}/")
    }
}

/// Synthesize results for tool calls that never got one (crash mid-tool, or a
/// suspend that was abandoned and re-prompted). Must run BEFORE new input is
/// appended so the synthetic results sit adjacent to their tool-call message.
fn repair_transcript(conn: &Connection, session: &str) -> Result<usize> {
    let rows: Vec<String> = {
        let mut stmt =
            conn.prepare("SELECT content FROM messages WHERE session_id=?1 ORDER BY id ASC")?;
        let r = stmt
            .query_map([session], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    let mut calls: Vec<(String, String)> = Vec::new(); // (call_id, name)
    let mut responded: HashSet<String> = HashSet::new();
    for raw in &rows {
        let m: Value = serde_json::from_str(raw)?;
        match m["role"].as_str().unwrap_or("") {
            "assistant" => {
                if let Some(tcs) = m["tool_calls"].as_array() {
                    for c in tcs {
                        calls.push((
                            c["call_id"].as_str().unwrap_or_default().to_string(),
                            c["fn_name"].as_str().unwrap_or_default().to_string(),
                        ));
                    }
                }
            }
            "tool" => {
                responded.insert(m["tool_call_id"].as_str().unwrap_or_default().to_string());
            }
            _ => {}
        }
    }
    let dangling: Vec<_> = calls.into_iter().filter(|(id, _)| !responded.contains(id)).collect();
    if dangling.is_empty() {
        return Ok(0);
    }
    for (call_id, name) in &dangling {
        store_msg(
            conn,
            session,
            None,
            &json!({
                "role": "tool",
                "tool_call_id": call_id,
                "name": name,
                "content": json!({ "error": "interrupted before a result was recorded" }).to_string(),
            }),
        )?;
    }
    // If the repaired call was a parked ask, that suspend is now abandoned.
    let key = pending_ask_key(session);
    if let Some(pend) = db::kv_get(conn, &key)? {
        let pend: Value = serde_json::from_str(&pend).unwrap_or(Value::Null);
        if dangling.iter().any(|(id, _)| Some(id.as_str()) == pend["call_id"].as_str()) {
            db::kv_del(conn, &key)?;
        }
    }
    Ok(dangling.len())
}

/// The session transcript as row-shaped JSON values — the seed of the
/// context document's `messages` (docs/context.md).
fn transcript_rows(conn: &Connection, session: &str) -> Result<Vec<Value>> {
    let mut stmt =
        conn.prepare("SELECT content FROM messages WHERE session_id=?1 ORDER BY id ASC")?;
    let raw = stmt
        .query_map([session], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raw.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
}

/// Build the genai chat request from the assembled (validated) document.
fn build_request(doc: &context::Doc, tools: &[Tool]) -> Result<ChatRequest> {
    let system = doc.system_text();
    let mut msgs: Vec<ChatMessage> = Vec::new();
    // Consecutive tool-result rows coalesce into ONE tool message: the
    // Anthropic protocol wants every tool_use answered by tool_result blocks
    // in the single next message, not a stack of messages.
    let mut pending_tools: Vec<ToolResponse> = Vec::new();
    fn flush_tools(msgs: &mut Vec<ChatMessage>, pending: &mut Vec<ToolResponse>) {
        if !pending.is_empty() {
            msgs.push(ChatMessage::from(std::mem::take(pending)));
        }
    }
    for m in &doc.messages {
        match m["role"].as_str().unwrap_or("") {
            "user" => {
                flush_tools(&mut msgs, &mut pending_tools);
                msgs.push(ChatMessage::user(m["text"].as_str().unwrap_or_default()));
            }
            "assistant" => {
                flush_tools(&mut msgs, &mut pending_tools);
                let text = m["text"].as_str().unwrap_or_default();
                if let Some(calls) = m["tool_calls"].as_array() {
                    // One assistant message with text + tool_use parts —
                    // splitting them into two messages breaks the protocol's
                    // "tool_result immediately after tool_use" rule.
                    let mut parts: Vec<ContentPart> = Vec::new();
                    if !text.is_empty() {
                        parts.push(ContentPart::Text(text.to_string()));
                    }
                    for c in calls {
                        parts.push(ContentPart::ToolCall(ToolCall {
                            call_id: c["call_id"].as_str().unwrap_or_default().to_string(),
                            fn_name: c["fn_name"].as_str().unwrap_or_default().to_string(),
                            fn_arguments: c["fn_arguments"].clone(),
                            thought_signatures: None,
                        }));
                    }
                    let content: MessageContent = parts.into_iter().collect();
                    msgs.push(ChatMessage::assistant(content));
                } else if !text.is_empty() {
                    msgs.push(ChatMessage::assistant(text));
                }
            }
            "tool" => {
                let tc = ToolCall {
                    call_id: m["tool_call_id"].as_str().unwrap_or_default().to_string(),
                    fn_name: m["name"].as_str().unwrap_or_default().to_string(),
                    fn_arguments: Value::Null,
                    thought_signatures: None,
                };
                pending_tools.push(ToolResponse::from_tool_call(
                    &tc,
                    m["content"].as_str().unwrap_or_default(),
                ));
            }
            _ => {}
        }
    }
    flush_tools(&mut msgs, &mut pending_tools);
    Ok(ChatRequest::new(msgs)
        .with_system(&system)
        .with_tools(tools.to_vec()))
}

fn tool_defs() -> Vec<Tool> {
    vec![
        Tool::new("shell")
            .with_description(
                "Run a shell command on the host via sh -c. Working directory is the profile's \
                 sandbox.workdir when set, else the harness root. \
                 Returns exit_code, stdout, stderr. Tools are the truth: prefer running a command over guessing.",
            )
            .with_schema(json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer", "description": "default 120" }
                },
                "required": ["command"]
            })),
        Tool::new("emit_event")
            .with_description(
                "Emit an event onto the harness bus. Handlers subscribed to its type run asynchronously; \
                 causality (cause_id) is threaded automatically. Use signal/ types for algedonic signals.",
            )
            .with_schema(json!({
                "type": "object",
                "properties": {
                    "type": { "type": "string", "description": "event topic, e.g. in/package/demo/echo or signal/pain" },
                    "payload": { "type": "object" },
                    "priority": { "type": "integer" }
                },
                "required": ["type"]
            })),
        Tool::new("fs_lease")
            .with_description(
                "Acquire an exclusive write lease (&mut) on a directory subtree before mutating it. \
                 The path must be an existing directory inside your granted write roots; the kernel is \
                 the borrow checker and refuses overlapping leases held by concurrent runs. Once you \
                 hold leases, shell commands can only write inside them (plus the harness root) — \
                 lease exactly what you intend to change. Leases release automatically when this run \
                 ends; there is no unlock to forget.",
            )
            .with_schema(json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "directory to lease, absolute or relative to the harness root" }
                },
                "required": ["path"]
            })),
        Tool::new("ask_human")
            .with_description(
                "Ask the human a question. Interactively this waits for an answer; under the daemon it \
                 suspends this run (checkpoint-and-exit) until the human answers or the deadline passes \
                 and the default applies. Prefer enumerated options; give a default + deadline_minutes \
                 whenever a sensible assumption exists.",
            )
            .with_schema(json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "options": { "type": "array", "items": { "type": "string" } },
                    "deadline_minutes": { "type": "number" },
                    "default": { "type": "string", "description": "what to assume if the deadline expires" }
                },
                "required": ["question"]
            })),
    ]
}

#[allow(clippy::too_many_arguments)]
fn run_tool(
    root: &Root,
    conn: &Connection,
    cage: &sandbox::Cage,
    prof: &profile::Profile,
    session: &str,
    event_id: Option<i64>,
    in_handler: bool,
    call: &ToolCall,
    self_emitted: &mut HashSet<i64>,
    mcp_pool: &crate::mcp::Pool,
) -> ToolOutcome {
    let args = &call.fn_arguments;
    let err = |msg: String| ToolOutcome::Output(json!({ "error": msg }).to_string());
    match call.fn_name.as_str() {
        "shell" => {
            let Some(cmd) = args["command"].as_str() else {
                return err("shell: missing 'command'".into());
            };
            let timeout = args["timeout_secs"].as_u64().unwrap_or(120);
            // Held leases narrow the cage: the spawn's write set becomes the
            // leases (plus the harness root) instead of the whole grant —
            // enforcement of exclusivity is the cage that exists anyway
            // (docs/sandbox.md). If the lease table can't be read we fail the
            // tool closed rather than run with the un-narrowed grant.
            let narrowed = match narrowed_cage(root, conn, cage) {
                Ok(n) => n,
                Err(e) => return err(format!("shell: cannot read active leases, refusing to run: {e:#}")),
            };
            let cage = narrowed.as_ref().unwrap_or(cage);
            // Workdir is location, not authority: resolved fresh per call so
            // a deleted dir fails loudly instead of falling back silently.
            let workdir = match resolve_workdir(&prof.sandbox) {
                Ok(w) => w,
                Err(e) => return err(format!("shell: {e:#}")),
            };
            // The camera: boundary diff of the writable roots around the
            // call. cause attribution is structural — this tool call IS the
            // bracket (docs/sandbox.md).
            let before = sandbox::snapshot(cage);
            let out = run_shell(root, cage, cmd, timeout, workdir.as_deref());
            let after = sandbox::snapshot(cage);
            emit_fs_delta(root, &prof.agent, session, &call.call_id, cage, &before, &after);
            ToolOutcome::Output(out)
        }
        "fs_lease" => {
            let Some(path) = args["path"].as_str() else {
                return err("fs_lease: missing 'path'".into());
            };
            match acquire_lease(root, conn, cage, &prof.agent, session, path) {
                Ok(leased) => ToolOutcome::Output(
                    json!({ "leased": leased, "held": held_leases(conn).unwrap_or_default() }).to_string(),
                ),
                Err(e) => err(format!("fs_lease: {e:#}")),
            }
        }
        "emit_event" => {
            let Some(etype) = args["type"].as_str() else {
                return err("emit_event: missing 'type'".into());
            };
            match events::emit(
                root,
                conn,
                EmitOpts {
                    payload: args.get("payload").filter(|p| !p.is_null()).cloned(),
                    priority: args["priority"].as_i64().unwrap_or(0),
                    cause: event_id,
                    ..EmitOpts::new(etype)
                },
            ) {
                Ok(id) => {
                    self_emitted.insert(id);
                    ToolOutcome::Output(json!({ "emitted_event_id": id }).to_string())
                }
                Err(e) => err(format!("emit failed: {e:#}")),
            }
        }
        "ask_human" => {
            let Some(question) = args["question"].as_str() else {
                return err("ask_human: missing 'question'".into());
            };
            let options: Vec<String> = args["options"]
                .as_array()
                .map(|a| a.iter().filter_map(|o| o.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if !in_handler {
                // Interactive: short-circuit through the terminal.
                if let Some(answer) = ask_tty(question, &options) {
                    return ToolOutcome::Output(json!({ "answer": answer }).to_string());
                }
            }
            // Daemon context: checkpoint-and-exit, never block.
            let corr = uuid::Uuid::new_v4().to_string();
            let deadline = args["deadline_minutes"].as_f64().map(|m| {
                (chrono::Utc::now() + chrono::Duration::seconds((m * 60.0) as i64))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            });
            let mut payload = json!({ "question": question, "session": session });
            if !options.is_empty() {
                payload["options"] = json!(options);
            }
            let ask_id = match events::emit(
                root,
                conn,
                EmitOpts {
                    payload: Some(payload),
                    correlation: Some(corr.clone()),
                    deadline,
                    default_action: args.get("default").filter(|d| !d.is_null()).cloned(),
                    cause: event_id,
                    // The ask is mail to the owner (docs/topics.md decided 6).
                    ..EmitOpts::new(&crate::topic::human_mailbox(&prof.owner))
                },
            ) {
                Ok(id) => id,
                Err(e) => return err(format!("ask emit failed: {e:#}")),
            };
            if let Err(e) = db::kv_set(
                conn,
                &pending_ask_key(session),
                &json!({ "call_id": call.call_id, "correlation": corr, "ask_id": ask_id }).to_string(),
            ) {
                return err(format!("checkpoint failed: {e:#}"));
            }
            let mut ids = trace::Ids::from_env();
            ids.session_id = Some(session.to_string());
            ids.correlation_id = Some(corr.clone());
            trace::write(
                root,
                &obs_tool(&prof.agent, session, "ask_human", "result"),
                &ids,
                json!({ "call_id": call.call_id, "name": "ask_human", "suspended": true, "ask_id": ask_id }),
            );
            eprintln!("suspending: waiting on human (ask #{ask_id}, correlation {corr})");
            ToolOutcome::Suspend
        }
        other => {
            // Namespaced MCP tools (<server>__<tool>) route to the pool; the
            // pool answers None only when no approved server claims the name.
            if let Some(out) = mcp_pool.call(other, args) {
                return ToolOutcome::Output(out);
            }
            err(format!("unknown tool: {other}"))
        }
    }
}

/// One obs/fs/ trace line per changed file plus a summary; exclusion is never
/// silent (the summary names the active patterns).
fn emit_fs_delta(
    root: &Root,
    agent: &str,
    session: &str,
    call_id: &str,
    cage: &sandbox::Cage,
    before: &sandbox::Snapshot,
    after: &sandbox::Snapshot,
) {
    let changes = sandbox::diff(before, after);
    if changes.is_empty() && !after.capped {
        return;
    }
    let mut ids = trace::Ids::from_env();
    ids.session_id = Some(session.to_string());
    for c in &changes {
        trace::write(
            root,
            &format!("obs/fs/{}", crate::topic::encode_path(&c.path)),
            &ids,
            json!({ "op": c.op, "size": c.size, "tool_call_id": call_id }),
        );
    }
    trace::write(
        root,
        &obs(agent, session, "fs/summary"),
        &ids,
        json!({
            "changed": changes.len(),
            "walk_capped": after.capped,
            "excluded_patterns": cage.exclude,
            "caged": cage.enforcing(),
            "tool_call_id": call_id,
        }),
    );
}

fn run_shell(
    root: &Root,
    cage: &sandbox::Cage,
    cmd: &str,
    timeout_secs: u64,
    workdir: Option<&std::path::Path>,
) -> String {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut c = cage.shell_command(cmd);
    c.current_dir(workdir.unwrap_or(&root.dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HARNESS_ROOT", &root.dir)
        .env("HARNESS_DB", root.db())
        .env("HARNESS_TRACE", root.trace_file());
    // Own process group so a timeout can kill the whole tree, not just sh.
    c.process_group(0);
    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(e) => return json!({ "error": format!("spawn failed: {e}") }).to_string(),
    };
    let pid = child.id() as i32;
    // Drain pipes concurrently: a child writing more than the pipe buffer
    // would otherwise block forever and look like a hang.
    let out_h = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut b = String::new();
            let _ = s.read_to_string(&mut b);
            b
        })
    });
    let err_h = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut b = String::new();
            let _ = s.read_to_string(&mut b);
            b
        })
    });
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) | Err(_) => {
                if Instant::now() > deadline {
                    unsafe {
                        libc::killpg(pid, libc::SIGKILL);
                    }
                    let _ = child.wait(); // reap; no zombies
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let stdout = out_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
    let stderr = err_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
    match status {
        Some(s) => json!({
            "exit_code": s.code().unwrap_or(-1),
            "stdout": trace::clip(&stdout, 8000),
            "stderr": trace::clip(&stderr, 4000),
        })
        .to_string(),
        None => json!({
            "error": format!("timed out after {timeout_secs}s (process group killed)"),
            "stdout": trace::clip(&stdout, 8000),
            "stderr": trace::clip(&stderr, 4000),
        })
        .to_string(),
    }
}

/// Interactive ask: read from /dev/tty so it works even when stdin carried the
/// prompt. Returns None when no terminal is available.
fn ask_tty(question: &str, options: &[String]) -> Option<String> {
    use std::io::{BufRead, BufReader, Write};
    let tty_in = std::fs::File::open("/dev/tty").ok()?;
    let mut tty_out = std::fs::OpenOptions::new().write(true).open("/dev/tty").ok()?;
    let _ = writeln!(tty_out, "\n[ask_human] {question}");
    if !options.is_empty() {
        let _ = writeln!(tty_out, "  options: {}", options.join(" | "));
    }
    let _ = write!(tty_out, "> ");
    let _ = tty_out.flush();
    let mut line = String::new();
    BufReader::new(tty_in).read_line(&mut line).ok()?;
    Some(line.trim().to_string())
}

/// Hourly token ceiling, keyed by the root cause's event type — agent-initiated
/// and human-initiated work get different budgets via throttle globs.
fn check_token_budget(root: &Root, conn: &Connection, root_type: &str, event_id: Option<i64>) -> Result<()> {
    let rows: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT event_type, llm_tokens_per_hour FROM throttles WHERE llm_tokens_per_hour IS NOT NULL",
        )?;
        let r = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    let limit = rows
        .iter()
        .filter(|(pat, _)| crate::topic::matches(pat, root_type))
        .map(|(_, l)| *l)
        .min();
    let Some(limit) = limit else { return Ok(()) };
    let used: i64 = conn.query_row(
        "SELECT COALESCE(SUM(input_tokens + output_tokens), 0) FROM llm_usage
         WHERE root_type = ?1 AND created_at > strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour')",
        [root_type],
        |r| r.get(0),
    )?;
    if used >= limit {
        events::emit(
            root,
            conn,
            EmitOpts {
                payload: Some(json!({
                    "reason": "llm token budget exhausted",
                    "root_type": root_type, "used": used, "limit": limit,
                })),
                cause: event_id,
                ..EmitOpts::new("signal/pain")
            },
        )?;
        bail!("llm token budget exhausted for {root_type}: {used}/{limit} in the last hour");
    }
    Ok(())
}

fn store_msg(conn: &Connection, session: &str, event_id: Option<i64>, msg: &Value) -> Result<()> {
    conn.execute(
        "INSERT INTO messages(session_id, role, content, event_id) VALUES (?1, ?2, ?3, ?4)",
        params![session, msg["role"].as_str().unwrap_or("?"), msg.to_string(), event_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::SandboxCfg;

    fn cfg(workdir: Option<&str>) -> SandboxCfg {
        SandboxCfg { workdir: workdir.map(String::from), ..Default::default() }
    }

    #[test]
    fn workdir_absent_means_none() {
        assert!(resolve_workdir(&cfg(None)).unwrap().is_none());
    }

    #[test]
    fn workdir_must_be_absolute() {
        let e = resolve_workdir(&cfg(Some("relative/path"))).unwrap_err();
        assert!(e.to_string().contains("absolute"), "{e}");
    }

    #[test]
    fn missing_workdir_fails_loudly_not_silently() {
        let p = std::env::temp_dir().join(format!("elanus-no-such-{}", uuid::Uuid::new_v4()));
        let e = resolve_workdir(&cfg(Some(&p.display().to_string()))).unwrap_err();
        assert!(e.to_string().contains("does not exist"), "{e}");
    }

    #[test]
    fn existing_workdir_resolves() {
        let p = std::env::temp_dir().join(format!("elanus-wd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        let got = resolve_workdir(&cfg(Some(&p.display().to_string()))).unwrap().unwrap();
        assert_eq!(got, p);
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn tilde_expands_against_home() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/x/y"), format!("{home}/x/y"));
        assert_eq!(expand_tilde("~"), home);
        // not a tilde prefix: untouched
        assert_eq!(expand_tilde("/a/~b"), "/a/~b");
    }
}

/// `elanus handle-exec` — the two-line-script backend for exec-as-handler.
/// Reads the event envelope from stdin per the handler contract.
pub fn handle_exec(root: &Root) -> Result<()> {
    let mut body = String::new();
    std::io::stdin().read_to_string(&mut body)?;
    let env: Value = serde_json::from_str(&body).context("handler stdin was not event JSON")?;
    let payload = &env["payload"];
    let session = payload["session"]
        .as_str()
        .map(String::from)
        .or_else(|| env["correlation_id"].as_str().map(|c| format!("evt-{c}")))
        .unwrap_or_else(|| format!("evt-{}", env["id"]));
    let profile = payload["profile"].as_str().unwrap_or("default").to_string();
    let resume = env.get("resume").filter(|r| !r.is_null());
    // The dispatching event rides into the context document verbatim
    // (docs/context.md): stages see topic, payload, correlation.
    let event = json!({
        "id": env["id"],
        "topic": env["type"],
        "payload": payload,
        "correlation_id": env["correlation_id"],
    });
    let opts = if let Some(r) = resume {
        let ans = match &r["payload"]["answer"] {
            Value::String(s) => s.clone(),
            Value::Null => "(no answer; deadline expired with no default)".to_string(),
            v => v.to_string(),
        };
        ExecOpts { session: Some(session), profile, prompt: None, resume: Some(ans), event: Some(event) }
    } else {
        let prompt = payload["prompt"].as_str().or_else(|| payload["text"].as_str());
        let Some(prompt) = prompt else {
            if !payload["answer"].is_null() {
                // An answer addressed to the agent mailbox (in/agent/<noun>)
                // shares the mailbox topic with ordinary work since v3; the
                // dispatcher's resume machinery already delivers it to the
                // suspended session by correlation, so this fresh dispatch of
                // the answer event itself is a no-op, not an error.
                eprintln!("[handle-exec] answer event; resume is correlation-driven, nothing to do");
                return Ok(());
            }
            bail!("agent mailbox (in/agent/<noun>) payload needs a 'prompt' (or 'text') field");
        };
        let prompt = prompt.to_string();
        ExecOpts { session: Some(session), profile, prompt: Some(prompt), resume: None, event: Some(event) }
    };
    run(root, opts)
}
