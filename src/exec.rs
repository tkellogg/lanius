use crate::config_repo;
use crate::context;
use crate::db;
use crate::envcompat::EnvDual;
use crate::events::{self, EmitOpts};
use crate::hooks;
use crate::paths::Root;
use crate::profile;
use crate::render;
use crate::sandbox;
use crate::trace;
use anyhow::{anyhow, bail, Context, Result};
use genai::chat::{
    ChatMessage, ChatRequest, ContentPart, MessageContent, Tool, ToolCall, ToolResponse,
};
use genai::Client;
use rumqttc::v5::mqttbytes::v5::Packet;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::{AsyncClient, Event, MqttOptions};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Read as _;
use std::time::Duration;

const CLIENT_TOOL_TIMEOUT: Duration = Duration::from_secs(120);

pub struct ExecOpts {
    pub session: Option<String>,
    pub profile: String,
    pub prompt: Option<String>,
    pub resume: Option<String>,
    /// The dispatching event for the context document's `event` field
    /// ({topic, payload, correlation_id}); None for CLI-direct runs.
    pub event: Option<Value>,
}

pub struct ContextRenderOpts {
    pub profile: String,
    pub session: String,
    pub event: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ClientToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
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

/// Developer inspection for the context program. This is intentionally
/// read-only with respect to the transcript: it reuses the same seed/chain
/// assembly as exec, but does not append an incoming event prompt to sqlite.
pub fn render_context(root: &Root, opts: ContextRenderOpts) -> Result<Value> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let (prof, _) = profile::load(root, &opts.profile)?;
    let (event_doc, event_prompt, event_source) =
        context_render_event(&conn, opts.event.as_deref())?;
    let system_seed = render::render_parts(root, &conn, &opts.profile, &opts.session)?;
    let stages = context::chain(root, &conn, &opts.profile, &prof)?;
    let mut messages = transcript_rows(&conn, &opts.session)?;
    if let Some(prompt) = event_prompt {
        if !event_already_in_transcript(&conn, &opts.session, &event_doc)? {
            messages.push(json!({ "role": "user", "text": prompt }));
        }
    }
    let assembly = context::assemble_detailed(
        root,
        &system_seed,
        messages,
        event_doc,
        context::Meta {
            profile: opts.profile.clone(),
            agent: prof.agent.clone(),
            session: opts.session.clone(),
            turn: 1,
            model: prof.model.model.clone(),
            vars: prof.vars.clone(),
        },
        &stages,
        None,
    )?;
    Ok(json!({
        "profile": opts.profile,
        "session": opts.session,
        "event_source": event_source,
        "seed": {
            "system_blocks": system_seed.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>(),
            "message_count": assembly.doc.messages.len(),
        },
        "resolved_stages": stages,
        "stage_summaries": assembly.stages,
        "document": assembly.doc,
    }))
}

fn context_render_event(
    conn: &Connection,
    arg: Option<&str>,
) -> Result<(Value, Option<String>, Value)> {
    let Some(raw) = arg else {
        return Ok((Value::Null, None, json!({ "kind": "none" })));
    };
    if let Ok(id) = raw.parse::<i64>() {
        let env = events::envelope(conn, id).with_context(|| format!("event {id} not found"))?;
        let prompt = event_prompt_from_payload(&env["payload"]);
        return Ok((
            envelope_to_context_event(&env),
            prompt,
            json!({ "kind": "event_id", "id": id }),
        ));
    }
    let value: Value =
        serde_json::from_str(raw).with_context(|| "--event must be an event id or JSON")?;
    if value.get("type").is_some() {
        let prompt = event_prompt_from_payload(&value["payload"]);
        return Ok((
            envelope_to_context_event(&value),
            prompt,
            json!({ "kind": "event_json", "shape": "envelope" }),
        ));
    }
    if value.get("topic").is_some() {
        let prompt = event_prompt_from_payload(&value["payload"]);
        return Ok((
            value,
            prompt,
            json!({ "kind": "event_json", "shape": "context_event" }),
        ));
    }
    let prompt = event_prompt_from_payload(&value);
    Ok((
        json!({ "payload": value }),
        prompt,
        json!({ "kind": "event_json", "shape": "payload" }),
    ))
}

fn envelope_to_context_event(env: &Value) -> Value {
    json!({
        "id": env["id"],
        "topic": env["type"],
        "payload": env["payload"],
        "correlation_id": env["correlation_id"],
        "sender": env["sender"],
    })
}

fn event_prompt_from_payload(payload: &Value) -> Option<String> {
    payload["prompt"]
        .as_str()
        .or_else(|| payload["text"].as_str())
        .map(ToString::to_string)
}

fn event_already_in_transcript(
    conn: &Connection,
    session: &str,
    event_doc: &Value,
) -> Result<bool> {
    let Some(id) = event_doc["id"].as_i64() else {
        return Ok(false);
    };
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE session_id=?1 AND event_id=?2",
        params![session, id],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// Wrap the turn so ANY failure of a correlated, dispatched run is reported
/// back on its correlation channel. The message was delivered and the agent
/// picked it up — the agent is what broke — so silence would strand the
/// client (Tim's call). The reply-mail success path already speaks this
/// channel; this is its failure twin. Best-effort and never masks the
/// original error: the dispatch still records the failure.
async fn run_async(root: &Root, opts: ExecOpts) -> Result<()> {
    let profile_name = opts.profile.clone();
    let session = opts.session.clone();
    // docs/config.md increment 3: a proposal-capable agent gets a disposable
    // clone of the config repo to edit. Set up before the turn, reap at the
    // terminal boundary (an ask_human suspend exits the process before here, by
    // design — that run harvests its proposal only when it finally terminates).
    let config_clone = config_clone_setup(root, &profile_name);
    let result = run_turn(root, opts).await;
    if let Err(e) = &result {
        report_agent_failure(root, &profile_name, session.as_deref(), e);
    }
    if let Some(clone) = &config_clone {
        config_clone_reap(root, &profile_name, clone);
    }
    result
}

/// docs/config.md increment 3 — set up the agent's config clone. A
/// proposal-capable agent (profile `autonomy != "off"`) gets a disposable clone
/// of the config repo in its run scratch (under `run/`, which is inside its cage
/// and NOT the fenced `config/`), and `$ELANUS_CONFIG_DIR` points at it. Returns
/// the clone path to reap, or None when the agent can't propose. Best-effort: a
/// clone failure disables proposals for the run, never breaks the run.
fn config_clone_setup(root: &Root, profile_name: &str) -> Option<std::path::PathBuf> {
    let prof = profile::load(root, profile_name).ok()?.0;
    if prof.autonomy == "off" {
        return None;
    }
    let dest = root
        .run_dir()
        .join(format!("exec-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest); // a crashed prior run may have left one
    if let Err(e) = config_repo::clone_for_agent(root, &dest) {
        eprintln!("[exec] config clone unavailable (proposals disabled this run): {e:#}");
        return None;
    }
    std::env::set_var("ELANUS_CONFIG_DIR", &dest);
    Some(dest)
}

/// docs/config.md increment 3 — reap the agent's clone at the terminal run
/// boundary: harvest any `proposal/*` branch into a pending proposal (a
/// `refs/proposals/<id>` ref + an `obs/config/proposed` ledger event attributed
/// to the agent), then delete the clone. Increment 4 layers autonomy auto-accept
/// on top of this.
fn config_clone_reap(root: &Root, profile_name: &str, clone: &std::path::Path) {
    let (agent, autonomy) = profile::load(root, profile_name)
        .map(|(p, _)| (p.agent, p.autonomy))
        .unwrap_or_default();
    let proposals = config_repo::reap_proposals(root, clone, &agent).unwrap_or_default();
    if !proposals.is_empty() {
        if let Ok(conn) = db::open(root) {
            for p in &proposals {
                // The proposal happened — record it (provenance: the agent).
                let _ = events::emit(
                    root,
                    &conn,
                    EmitOpts {
                        payload: Some(json!({
                            "proposal": p.id,
                            "agent": agent,
                            "branch": p.branch,
                            "files": p.files,
                            "commit": p.commit,
                        })),
                        sender: Some(agent.clone()),
                        ..EmitOpts::new("obs/config/proposed")
                    },
                );
                // Autonomy (docs/config.md D4): auto-accept only what the agent's
                // level allows; everything else waits for a person.
                match crate::configcli::classify(root, &p.id, &autonomy) {
                    crate::configcli::Verdict::Accept => {
                        if let Ok(sha) = config_repo::accept_proposal(root, &p.id) {
                            let _ = events::emit(
                                root,
                                &conn,
                                EmitOpts {
                                    payload: Some(json!({
                                        "proposal": p.id,
                                        "packages": p.files,
                                        "commit": sha,
                                        "decided_by": format!("autonomy:{autonomy}"),
                                        "agent": agent,
                                        "via": "autonomy",
                                    })),
                                    sender: Some(agent.clone()),
                                    ..EmitOpts::new("obs/config/changed")
                                },
                            );
                        }
                    }
                    crate::configcli::Verdict::Hold(reason) => {
                        eprintln!(
                            "[config] proposal {} held under autonomy {autonomy}: {reason}",
                            p.id
                        );
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(clone);
}

/// Emit a labeled failure to the human mailbox for a dispatched, correlated
/// run. `failed: true` so any client (web, TUI, …) threads it by
/// correlation and dedups against the run that produced it. CLI-direct runs
/// (no event/correlation) are skipped — their error already hit the
/// terminal.
fn report_agent_failure(
    root: &Root,
    profile_name: &str,
    session: Option<&str>,
    err: &anyhow::Error,
) {
    let ids = trace::Ids::from_env();
    let (Some(event_id), Some(corr)) = (ids.event_id, ids.correlation_id.clone()) else {
        return;
    };
    let Ok(conn) = db::open(root) else { return };
    let (prof, _) = match profile::load(root, profile_name) {
        Ok(p) => p,
        Err(_) => return,
    };
    let reason = trace::clip(&format!("{err:#}"), 800);
    let _ = events::emit(
        root,
        &conn,
        EmitOpts {
            payload: Some(json!({
                "failed": true,
                "error": reason,
                "agent": prof.agent,
                "session": session,
            })),
            correlation: Some(corr),
            cause: Some(event_id),
            ..EmitOpts::new(&crate::topic::human_mailbox(&prof.owner))
        },
    );
}

async fn run_turn(root: &Root, opts: ExecOpts) -> Result<()> {
    let conn = db::open(root)?;
    db::init_schema(&conn)?;
    let (prof, _pdir) = profile::load(root, &opts.profile)?;
    // Provenance (docs/identity.md): events this run emits — its reply mail,
    // failure mail, ask_human, any emit_event tool call, and anything the
    // shell tool runs via `elanus emit` — attribute to the agent. This is
    // self-reported (the run writes the ledger directly) until the ledger
    // becomes kernel-only-writable; the broker-verified path is the
    // unforgeable one.
    // Canonical name + legacy alias, so events::emit (and any child reading
    // either) attributes to this agent.
    std::env::set_var("ELANUS_ACTOR", &prof.agent);
    std::env::set_var("HARNESS_ACTOR", &prof.agent);
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
            store_msg(
                &conn,
                &session,
                event_id,
                &json!({ "role": "user", "text": p }),
            )?;
        } else {
            bail!("nothing to do: provide a prompt, '-' for stdin, or --resume");
        }
    }

    // Context pipeline (docs/context.md): the system SEED is computed once
    // per run (blocks + providers + skills inventory — pre-pipeline
    // semantics, providers don't re-spawn per call); the chain runs before
    // every LLM call over the freshly re-read transcript.
    let system_seed = render::render_parts(root, &conn, &opts.profile, &session)?;
    let stages = context::chain(root, &conn, &opts.profile, &prof)?;
    let event_doc = opts.event.clone().unwrap_or(Value::Null);
    let client_tools = client_tools_from_event(&event_doc);
    // The cage is built once per exec from the profile's [sandbox] grant;
    // every shell tool call spawns inside it and gets boundary-diffed.
    let cage = sandbox::Cage::from_profile(root, &prof.sandbox);
    let (client, chat_opts) = build_client(root, &conn, &prof)?;
    let model = prof.model.model.clone();
    // MCP servers (border protocol, src/mcp.rs): approved third-party tool
    // servers spawn inside the agent's cage for the run; their tools join
    // the array as <server>__<tool>. Failures degrade loudly, never fatal.
    let mcp_pool = crate::mcp::Pool::load(root, &conn, &opts.profile, &prof, &cage);
    let mut tools = tool_defs();
    // M3 (docs/handoffs/chat-rendering.md): a built-in tool a package "owns"
    // (declares in `provides_builtin_tools`) is available only when a package
    // providing it is visible to this profile. So a worker subagent that drops
    // the comms package (e.g. `inherit_to_subagents = false` under `$parent`)
    // actually loses `send_message`/`ask_human`, not merely the etiquette text.
    let withheld = crate::packages::withheld_builtin_tools(root, &opts.profile);
    if !withheld.is_empty() {
        tools.retain(|t| !withheld.contains(t.name.as_str()));
    }
    let client_tool_names: HashSet<String> = client_tools.iter().map(|t| t.name.clone()).collect();
    tools.extend(client_tool_defs(&client_tools));
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
            bail!(
                "max_turns ({}) reached for session {session}",
                prof.model.max_turns
            );
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
                vars: prof.vars.clone(),
            },
            &stages,
            &ids,
        )?;
        let chat_req = build_request(&doc, &tools)?;
        trace::write(
            root,
            &obs(&prof.agent, &session, "llm/request"),
            &ids,
            json!({ "model": model, "turn": turns }),
        );
        let res = client
            .exec_chat(model.as_str(), chat_req, chat_opts.as_ref())
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
        // Reasoning is RECORDED, never replayed: the transcript is the
        // flight recorder's truth (and the history DSL projects it), but
        // build_request ignores it on the wire — DeepSeek rejects echoed
        // reasoning_content, and Anthropic thinking replay only matters
        // once thinking mode is actually enabled in requests.
        let reasoning = res.content.reasoning_contents().join("\n\n");
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
                "reasoning": (!reasoning.is_empty()).then(|| trace::clip(&reasoning, 2000)),
            }),
        );

        let mut amsg = json!({ "role": "assistant" });
        if let Some(t) = &text {
            amsg["text"] = json!(t);
        }
        if !reasoning.is_empty() {
            amsg["reasoning"] = json!(reasoning);
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
                crate::resident::consult(
                    root,
                    &conn,
                    "pre_tool_call",
                    &call.fn_name,
                    pre.subject,
                    &ids,
                )
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
            if !pre.allow {
                // The (effective) tool call goes to the trace BEFORE execution:
                // a crash mid-tool must be visible as a call with no result.
                trace::write(
                    root,
                    &obs_tool(&prof.agent, &session, &eff.fn_name, "call"),
                    &ids,
                    json!({ "call_id": eff.call_id, "name": eff.fn_name, "args": eff.fn_arguments }),
                );
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
            let outcome = if client_tool_names.contains(&eff.fn_name) {
                run_client_tool(root, &prof, &session, event_id, &ids, &eff).await
            } else {
                // The (effective) tool call goes to the trace BEFORE execution:
                // a crash mid-tool must be visible as a call with no result.
                trace::write(
                    root,
                    &obs_tool(&prof.agent, &session, &eff.fn_name, "call"),
                    &ids,
                    json!({ "call_id": eff.call_id, "name": eff.fn_name, "args": eff.fn_arguments }),
                );
                run_tool(
                    root,
                    &conn,
                    &cage,
                    &prof,
                    &session,
                    event_id,
                    ids.correlation_id.as_deref(),
                    in_handler,
                    &eff,
                    &mut self_emitted,
                    &mcp_pool,
                )
            };
            match outcome {
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
                        crate::resident::consult(
                            root,
                            &conn,
                            "post_tool_call",
                            &eff.fn_name,
                            post.subject,
                            &ids,
                        )
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
                json!({ "error": "interrupted: run suspended while waiting on the human" })
                    .to_string();
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
                .query_map([signal_watermark], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            r
        };
        signal_watermark = sigs.iter().map(|s| s.0).max().unwrap_or(signal_watermark);
        let sigs: Vec<_> = sigs
            .into_iter()
            .filter(|(id, _, _)| !self_emitted.contains(id))
            .collect();
        if !sigs.is_empty() {
            let note = sigs
                .iter()
                .map(|(id, t, p)| format!("[signal #{id}] {t} {}", p.as_deref().unwrap_or("{}")))
                .collect::<Vec<_>>()
                .join("\n");
            trace::write(
                root,
                &obs(&prof.agent, &session, "signal/injected"),
                &ids,
                json!({ "injected": sigs.len() }),
            );
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

/// A message an actor sends to a channel — the one comms primitive
/// (docs/handoffs/chat-rendering.md). `send_message` and `ask_human` are the
/// same emit: write to a channel inbox, threaded by correlation. The only
/// difference is run-scheduling — `ask` suspends and parks a `pending_ask`,
/// `send_message` keeps working — so that decision lives in the caller, not
/// here. This is the single emit path / single correlation discipline the
/// milestone requires.
struct OutboundMessage {
    /// Channel inbox topic (default = the owner's mailbox, in/human/<owner>).
    topic: String,
    /// The message body; `text` for a plain message, `question`/`options` for
    /// an ask. Already shaped by the caller.
    payload: Value,
    /// Threading id. An ask mints a fresh correlation (it parks on it); a
    /// send threads onto the turn's correlation when one exists so the reply
    /// continues the conversation instead of dead-ending.
    correlation: Option<String>,
    /// Ask-only: deadline + default-on-expiry. None for a fire-and-forget send.
    deadline: Option<String>,
    default_action: Option<Value>,
}

/// The shared emit path for the send/ask family. Emits the message to its
/// channel and returns the new event id. Both verbs route through here so
/// there is exactly one place that decides how a message reaches a channel.
fn emit_message(
    root: &Root,
    conn: &Connection,
    cause: Option<i64>,
    msg: &OutboundMessage,
) -> Result<i64> {
    events::emit(
        root,
        conn,
        EmitOpts {
            payload: Some(msg.payload.clone()),
            correlation: msg.correlation.clone(),
            deadline: msg.deadline.clone(),
            default_action: msg.default_action.clone(),
            cause,
            ..EmitOpts::new(&msg.topic)
        },
    )
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
    obs(
        agent,
        session,
        &format!("tool/{}/{leaf}", crate::topic::encode_segment(tool)),
    )
}

// ── Leases: &mut on subtrees, the kernel as borrow checker ─────────────────
// (docs/sandbox.md). Lease lifetime = holder lifetime: the dispatcher
// releases leases of finished dispatches and dead pids; a standalone exec
// releases its own on clean exit. There is no unlock call to forget.

/// Who holds leases acquired by this process: the enclosing dispatch when
/// there is one (survives suspend/resume), else this pid.
fn lease_holder() -> (Option<i64>, i64) {
    let dispatch = crate::envcompat::read("DISPATCH_ID").and_then(|v| v.parse().ok());
    (dispatch, std::process::id() as i64)
}

fn same_holder(
    dispatch: Option<i64>,
    pid: i64,
    row_dispatch: Option<i64>,
    row_pid: Option<i64>,
) -> bool {
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
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.dir.join(p)
    };
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
            cage.write_roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
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
            let mut stmt = conn
                .prepare("SELECT path, dispatch_id, pid FROM leases WHERE released_at IS NULL")?;
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
        &trace::Ids {
            session_id: Some(session.into()),
            ..Default::default()
        },
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
fn narrowed_cage(
    root: &Root,
    conn: &Connection,
    base: &sandbox::Cage,
) -> anyhow::Result<Option<sandbox::Cage>> {
    let held = held_leases(conn)?;
    if held.is_empty() {
        return Ok(None);
    }
    let mut roots = vec![root.dir.clone()];
    roots.extend(held.into_iter().map(std::path::PathBuf::from));
    Ok(Some(sandbox::Cage::from_roots(
        roots,
        base.exclude.clone(),
        true,
        &sandbox::Protect::for_root(root),
    )))
}

/// Resolve the profile's `[sandbox] workdir`: tilde-expanded, must be an
/// absolute path to an existing directory. None = run in the harness root,
/// as ever. This is *location*, not authority — writes still flow through
/// the whole-agent grant + leases; the cage is unchanged by it.
fn resolve_workdir(cfg: &profile::SandboxCfg) -> anyhow::Result<Option<std::path::PathBuf>> {
    let Some(w) = &cfg.workdir else {
        return Ok(None);
    };
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

/// How `build_client` will configure the genai client for this run. Extracted as
/// a testable seam: genai's resolved `ServiceTarget` is produced inside a closure
/// and isn't directly inspectable, so the resolution DECISION is computed here and
/// asserted in tests, while `build_client` only translates a plan into a `Client`.
#[derive(Debug)]
enum DispatcherPlan {
    /// No override in play — genai's defaults (adapter auth + endpoint).
    Default,
    /// DEPRECATED inline path (`[model].base_url`/`api_key_env`), unchanged: an
    /// endpoint override (profile base_url, or ANTHROPIC_BASE_URL for the
    /// Anthropic adapter) and/or an env-var-named API key.
    Inline {
        profile_url: Option<String>,
        env_url: Option<String>,
        api_key_env: Option<String>,
    },
    /// Canonical path: a named provider resolved through the vault. Carries the
    /// LITERAL coordinates (endpoint + decrypted key + extra headers) — wins over
    /// any inline fields. Never logged (the key is a `Secret`).
    Provider(crate::provider::DispatcherInjection),
}

/// Map a provider wire to the genai adapter whose endpoint-normalization rules
/// (and request shape) match it. The provider's wire is authoritative for the
/// canonical path — it decides how the base_url is normalized.
fn wire_adapter(wire: crate::provider::Wire) -> genai::adapter::AdapterKind {
    use genai::adapter::AdapterKind;
    match wire {
        crate::provider::Wire::Anthropic => AdapterKind::Anthropic,
        crate::provider::Wire::OpenAI => AdapterKind::OpenAI,
    }
}

/// Decide how to configure the dispatcher client for this profile. The testable
/// seam: `env_url` is threaded in (the read-of-ANTHROPIC_BASE_URL) so the
/// decision is pure given (root, conn, profile, env). A named `[model].provider`
/// wins wholesale; a `NativeLogin` provider returns a legible Err (the agent
/// can't start — there is no secret to feed a genai client); an unknown provider
/// name errors. With no provider, the existing inline behavior is preserved
/// byte-for-byte.
fn dispatcher_plan(
    root: &Root,
    conn: &Connection,
    prof: &profile::Profile,
    env_url: Option<String>,
) -> Result<DispatcherPlan> {
    if let Some(name) = prof.model.provider.as_deref() {
        let provider = crate::provider::get(root, conn, name)?.ok_or_else(|| {
            anyhow!(
                "profile's [model].provider names {name:?}, but no such provider exists — \
                 define it with `elanus provider add {name} …` (or remove [model].provider \
                 to use the inline base_url/api_key_env)"
            )
        })?;
        // Dispatcher consumer: ApiKey -> DispatcherInjection; NativeLogin -> the
        // legible refusal, propagated so the agent fails to start with a clear
        // message (a native login can't drive the genai dispatcher).
        let inj = crate::provider::materialize(
            name,
            &provider.credential,
            crate::provider::Consumer::Dispatcher,
            None,
        )?;
        let crate::provider::Injection::Dispatcher(d) = inj else {
            unreachable!("the Dispatcher consumer yields a Dispatcher injection");
        };
        return Ok(DispatcherPlan::Provider(d));
    }

    let profile_url = prof.model.base_url.clone();
    let api_key_env = prof.model.api_key_env.clone();
    if profile_url.is_none() && env_url.is_none() && api_key_env.is_none() {
        return Ok(DispatcherPlan::Default);
    }
    Ok(DispatcherPlan::Inline {
        profile_url,
        env_url,
        api_key_env,
    })
}

/// Default client unless an endpoint/auth override is in play — then a
/// ServiceTargetResolver rewrites the target.
///
/// Two override sources, in precedence order:
/// - **A named provider** (`[model].provider`, the canonical path): the vault
///   supplies the literal endpoint + key (and any extra headers). It WINS over
///   the inline fields. Extra headers (LiteLLM/OpenRouter) are returned as a
///   PER-CALL `ChatOptions` (the second tuple element), NOT set client-wide:
///   genai 0.6.5's Anthropic/OpenAI adapters merge `extra_headers` only from the
///   per-call `options` argument of `exec_chat` (client_impl.rs:110), never from
///   the client config — so client-level `with_extra_headers` would be silently
///   dropped for exactly the adapters elanus uses. The caller threads the
///   returned options into the `exec_chat(..., opts.as_ref())` call so the
///   headers actually reach the wire (additive — preserves the adapter's auth
///   header).
/// - **Inline `base_url`/`api_key_env`** (DEPRECATED, unchanged): ANTHROPIC_BASE_URL
///   (env) only applies when the model resolved to the Anthropic adapter,
///   mirroring the Anthropic SDK; a profile's explicit base_url applies
///   unconditionally; api_key_env reads the key from a named env var.
///
/// Returns the configured `Client` and an optional per-call `ChatOptions`
/// carrying the provider's extra headers (`Some` only when non-empty).
fn build_client(
    root: &Root,
    conn: &Connection,
    prof: &profile::Profile,
) -> Result<(Client, Option<genai::chat::ChatOptions>)> {
    use genai::adapter::AdapterKind;
    use genai::chat::ChatOptions;
    use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
    use genai::ServiceTarget;

    let env_url = std::env::var("ANTHROPIC_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty());

    match dispatcher_plan(root, conn, prof, env_url)? {
        DispatcherPlan::Default => Ok((Client::default(), None)),

        DispatcherPlan::Inline {
            profile_url,
            env_url,
            api_key_env,
        } => {
            let resolver = ServiceTargetResolver::from_resolver_fn(
                move |mut target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
                    let adapter = target.model.adapter_kind;
                    let url = profile_url.clone().or_else(|| {
                        (adapter == AdapterKind::Anthropic)
                            .then(|| env_url.clone())
                            .flatten()
                    });
                    if let Some(url) = url {
                        target.endpoint = Endpoint::from_owned(normalize_base_url(&url, adapter));
                    }
                    if let Some(envk) = &api_key_env {
                        target.auth = AuthData::from_env(envk.clone());
                    }
                    Ok(target)
                },
            );
            Ok((
                Client::builder()
                    .with_service_target_resolver(resolver)
                    .build(),
                None,
            ))
        }

        DispatcherPlan::Provider(inj) => {
            // The provider's wire is authoritative: it decides how the base_url is
            // normalized and (for an Anthropic provider with an Anthropic model)
            // matches genai's request shape.
            let adapter = wire_adapter(inj.wire);
            let endpoint = normalize_base_url(&inj.base_url, adapter);
            // The LITERAL decrypted key (the vault stores the secret itself now,
            // not an env-var name). Materialized transiently into the resolver;
            // never logged.
            let key = inj.key.expose().to_string();
            let resolver = ServiceTargetResolver::from_resolver_fn(
                move |mut target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
                    target.endpoint = Endpoint::from_owned(endpoint.clone());
                    target.auth = AuthData::from_single(key.clone());
                    Ok(target)
                },
            );
            let client = Client::builder()
                .with_adapter_kind(adapter)
                .with_service_target_resolver(resolver)
                .build();
            // Extra headers (LiteLLM/OpenRouter): additive on top of the adapter's
            // auth header. Carried as PER-CALL ChatOptions — genai 0.6.5's
            // Anthropic/OpenAI adapters merge `extra_headers` only from the per-call
            // `options` argument of exec_chat (client_impl.rs:110), never from the
            // client config, so a client-level `with_extra_headers` would be silently
            // dropped for those adapters. The caller threads this into the exec_chat
            // call so the headers reach the wire.
            let chat_opts = (!inj.headers.is_empty()).then(|| {
                let pairs: Vec<(String, String)> = inj
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.expose().to_string()))
                    .collect();
                ChatOptions::default().with_extra_headers(pairs)
            });
            Ok((client, chat_opts))
        }
    }
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
    let dangling: Vec<_> = calls
        .into_iter()
        .filter(|(id, _)| !responded.contains(id))
        .collect();
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
        if dangling
            .iter()
            .any(|(id, _)| Some(id.as_str()) == pend["call_id"].as_str())
        {
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
        Tool::new("send_message")
            .with_description(
                "Send a message to a channel — by default the owner's mailbox (in/human/<owner>). \
                 Use this to speak UNPROMPTED: surface something worth the human's attention, share \
                 progress, or report a result, WITHOUT pausing your run. It does NOT suspend and does \
                 NOT wait for a reply — you keep working. If the human replies it arrives as ordinary \
                 inbound mail on the same thread. When you actually need an answer before you can \
                 continue, use `ask_human` instead (that one blocks). Speak to feel alive, not to spam: \
                 send when it earns the interruption.",
            )
            .with_schema(json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "the message to send" }
                },
                "required": ["text"]
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

fn client_tool_defs(defs: &[ClientToolDef]) -> Vec<Tool> {
    defs.iter()
        .filter(|d| !d.name.trim().is_empty())
        .map(|d| {
            Tool::new(&d.name)
                .with_description(d.description.as_str())
                .with_schema(d.parameters.clone())
        })
        .collect()
}

fn client_tools_from_event(event: &Value) -> Vec<ClientToolDef> {
    event["payload"]["client_tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| {
                    let name = t["name"].as_str()?.trim();
                    if name.is_empty() {
                        return None;
                    }
                    Some(ClientToolDef {
                        name: name.to_string(),
                        description: t["description"].as_str().unwrap_or("").to_string(),
                        parameters: t
                            .get("parameters")
                            .cloned()
                            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn run_client_tool(
    root: &Root,
    prof: &profile::Profile,
    session: &str,
    event_id: Option<i64>,
    ids: &trace::Ids,
    call: &ToolCall,
) -> ToolOutcome {
    let result_topic = obs_tool(&prof.agent, session, &call.fn_name, "result");
    let call_topic = obs_tool(&prof.agent, session, &call.fn_name, "call");
    let (ready, mut results) =
        subscribe_client_tool_result(root.clone(), result_topic.clone(), call.call_id.clone());

    match tokio::time::timeout(CLIENT_TOOL_TIMEOUT, ready).await {
        Err(_) => {
            let result = json!({ "error": format!("client tool result subscription timed out after {}s", CLIENT_TOOL_TIMEOUT.as_secs()) }).to_string();
            trace::write(
                root,
                &result_topic,
                ids,
                json!({ "call_id": call.call_id, "name": call.fn_name, "timeout": true, "result": trace::clip(&result, 2000) }),
            );
            let _ = event_id;
            return ToolOutcome::Output(result);
        }
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => {
            let result = json!({ "error": e }).to_string();
            trace::write(
                root,
                &result_topic,
                ids,
                json!({ "call_id": call.call_id, "name": call.fn_name, "error": trace::clip(&result, 2000) }),
            );
            let _ = event_id;
            return ToolOutcome::Output(result);
        }
        Ok(Err(_)) => {
            let result =
                json!({ "error": "client tool result listener stopped before subscribing" })
                    .to_string();
            trace::write(
                root,
                &result_topic,
                ids,
                json!({ "call_id": call.call_id, "name": call.fn_name, "error": trace::clip(&result, 2000) }),
            );
            let _ = event_id;
            return ToolOutcome::Output(result);
        }
    }

    trace::write(
        root,
        &obs_tool(&prof.agent, session, &call.fn_name, "await"),
        ids,
        json!({ "call_id": call.call_id, "name": call.fn_name, "client_tool": true, "timeout_ms": CLIENT_TOOL_TIMEOUT.as_millis() }),
    );
    // Client tools publish their browser-visible call only after the result
    // subscription is active, so a fast handler cannot beat the waiter.
    trace::write(
        root,
        &call_topic,
        ids,
        json!({ "call_id": call.call_id, "name": call.fn_name, "args": call.fn_arguments }),
    );

    match await_client_tool_result(&mut results, &call.call_id).await {
        Ok(Some(Ok(result))) => ToolOutcome::Output(result),
        Ok(Some(Err(e))) => {
            let result = json!({ "error": e }).to_string();
            trace::write(
                root,
                &result_topic,
                ids,
                json!({ "call_id": call.call_id, "name": call.fn_name, "error": trace::clip(&result, 2000) }),
            );
            let _ = event_id;
            ToolOutcome::Output(result)
        }
        Ok(None) => {
            let result = json!({ "error": "client tool result listener stopped" }).to_string();
            trace::write(
                root,
                &result_topic,
                ids,
                json!({ "call_id": call.call_id, "name": call.fn_name, "error": trace::clip(&result, 2000) }),
            );
            let _ = event_id;
            ToolOutcome::Output(result)
        }
        Err(e) => {
            let result = json!({ "error": e }).to_string();
            trace::write(
                root,
                &result_topic,
                ids,
                json!({ "call_id": call.call_id, "name": call.fn_name, "timeout": true, "result": trace::clip(&result, 2000) }),
            );
            let _ = event_id;
            ToolOutcome::Output(result)
        }
    }
}

async fn await_client_tool_result(
    rx: &mut tokio::sync::mpsc::Receiver<std::result::Result<String, String>>,
    call_id: &str,
) -> std::result::Result<Option<std::result::Result<String, String>>, String> {
    tokio::time::timeout(CLIENT_TOOL_TIMEOUT, rx.recv())
        .await
        .map_err(|_| {
            format!(
                "client tool {call_id} timed out after {}s",
                CLIENT_TOOL_TIMEOUT.as_secs()
            )
        })
}

fn subscribe_client_tool_result(
    root: Root,
    topic: String,
    call_id: String,
) -> (
    tokio::sync::oneshot::Receiver<std::result::Result<(), String>>,
    tokio::sync::mpsc::Receiver<std::result::Result<String, String>>,
) {
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (result_tx, result_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        let mut ready_tx = Some(ready_tx);
        let cfg = crate::bus::config(&root);
        let Some(addr) = crate::bus::connect_addr(&cfg) else {
            let _ = ready_tx
                .take()
                .unwrap()
                .send(Err(format!("unparseable bus bind address {:?}", cfg.bind)));
            return;
        };
        let mut opts = MqttOptions::new(
            format!(
                "el-exec-tool-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ),
            addr.ip().to_string(),
            addr.port(),
        );
        opts.set_keep_alive(Duration::from_secs(10));
        opts.set_max_packet_size(Some(crate::resident::MAX_PACKET));
        if let Some(secret) = crate::secrets::read(&root, crate::secrets::KERNEL) {
            opts.set_credentials(crate::secrets::KERNEL, secret);
        }

        let (client, mut eventloop) = AsyncClient::new(opts, 16);
        if let Err(e) = client.subscribe(&topic, QoS::AtLeastOnce).await {
            let _ = ready_tx
                .take()
                .unwrap()
                .send(Err(format!("client tool result subscribe failed: {e}")));
            return;
        }

        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::SubAck(_))) => {
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(Ok(()));
                    }
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    let msg = format!("client tool result subscription failed: {e}");
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(Err(msg));
                    } else {
                        let _ = result_tx.send(Err(msg)).await;
                    }
                    return;
                }
            }
        }

        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::Publish(p))) => {
                    if let Some(result) = client_tool_result_from_payload(&p.payload, &call_id) {
                        let _ = result_tx.send(Ok(result)).await;
                        return;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    let _ = result_tx
                        .send(Err(format!("client tool result receive failed: {e}")))
                        .await;
                    return;
                }
            }
        }
    });
    (ready_rx, result_rx)
}

fn client_tool_result_from_payload(raw: &[u8], call_id: &str) -> Option<String> {
    let raw_payload: Value = serde_json::from_slice(raw).unwrap_or(Value::Null);
    let payload = raw_payload
        .get("payload")
        .filter(|v| v.is_object())
        .unwrap_or(&raw_payload);
    if payload["call_id"].as_str() != Some(call_id) {
        return None;
    }
    if payload["cancelled"].as_bool() == Some(true) {
        return Some(json!({ "error": "client tool cancelled", "cancelled": true }).to_string());
    }
    if let Some(err) = payload["error"].as_str() {
        return Some(json!({ "error": err }).to_string());
    }
    Some(match payload.get("result") {
        Some(Value::String(s)) => s.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => Value::Null.to_string(),
    })
}

#[allow(clippy::too_many_arguments)]
fn run_tool(
    root: &Root,
    conn: &Connection,
    cage: &sandbox::Cage,
    prof: &profile::Profile,
    session: &str,
    event_id: Option<i64>,
    turn_correlation: Option<&str>,
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
                Err(e) => {
                    return err(format!(
                        "shell: cannot read active leases, refusing to run: {e:#}"
                    ))
                }
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
            emit_fs_delta(
                root,
                &prof.agent,
                session,
                &call.call_id,
                cage,
                &before,
                &after,
            );
            ToolOutcome::Output(out)
        }
        "fs_lease" => {
            let Some(path) = args["path"].as_str() else {
                return err("fs_lease: missing 'path'".into());
            };
            match acquire_lease(root, conn, cage, &prof.agent, session, path) {
                Ok(leased) => ToolOutcome::Output(
                    json!({ "leased": leased, "held": held_leases(conn).unwrap_or_default() })
                        .to_string(),
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
        "send_message" => {
            let Some(text) = args["text"].as_str() else {
                return err("send_message: missing 'text'".into());
            };
            // send = the non-suspending mode of the send family: emit through
            // the one shared path and KEEP WORKING (no pending_ask, no
            // ToolOutcome::Suspend). Thread onto the turn's correlation when
            // there is one so a reply continues the conversation instead of
            // dead-ending; uncorrelated (CLI-direct) sends just land on the
            // mailbox. Default channel = the owner's mailbox.
            let msg_id = match emit_message(
                root,
                conn,
                event_id,
                &OutboundMessage {
                    topic: crate::topic::human_mailbox(&prof.owner),
                    payload: json!({ "text": text, "session": session }),
                    correlation: turn_correlation.map(String::from),
                    deadline: None,
                    default_action: None,
                },
            ) {
                Ok(id) => id,
                Err(e) => return err(format!("send_message emit failed: {e:#}")),
            };
            self_emitted.insert(msg_id);
            let mut ids = trace::Ids::from_env();
            ids.session_id = Some(session.to_string());
            if let Some(c) = turn_correlation {
                ids.correlation_id = Some(c.to_string());
            }
            trace::write(
                root,
                &obs_tool(&prof.agent, session, "send_message", "result"),
                &ids,
                json!({ "call_id": call.call_id, "name": "send_message", "suspended": false, "message_id": msg_id }),
            );
            ToolOutcome::Output(json!({ "sent": true, "message_id": msg_id }).to_string())
        }
        "ask_human" => {
            let Some(question) = args["question"].as_str() else {
                return err("ask_human: missing 'question'".into());
            };
            let options: Vec<String> = args["options"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|o| o.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if !in_handler {
                // Interactive: short-circuit through the terminal.
                if let Some(answer) = ask_tty(question, &options) {
                    return ToolOutcome::Output(json!({ "answer": answer }).to_string());
                }
            }
            // Daemon context: checkpoint-and-exit, never block.
            // ask = the suspend=true / expects_reply=true MODE of the send
            // family: it mints a fresh correlation (it parks on it) and emits
            // through the one shared path (docs/handoffs/chat-rendering.md).
            let corr = uuid::Uuid::new_v4().to_string();
            let deadline = args["deadline_minutes"].as_f64().map(|m| {
                (chrono::Utc::now() + chrono::Duration::seconds((m * 60.0) as i64))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            });
            let mut payload = json!({ "question": question, "session": session });
            if !options.is_empty() {
                payload["options"] = json!(options);
            }
            let ask_id = match emit_message(
                root,
                conn,
                event_id,
                &OutboundMessage {
                    // The ask is mail to the owner (docs/topics.md decided 6).
                    topic: crate::topic::human_mailbox(&prof.owner),
                    payload,
                    correlation: Some(corr.clone()),
                    deadline,
                    default_action: args.get("default").filter(|d| !d.is_null()).cloned(),
                },
            ) {
                Ok(id) => id,
                Err(e) => return err(format!("ask emit failed: {e:#}")),
            };
            if let Err(e) = db::kv_set(
                conn,
                &pending_ask_key(session),
                &json!({ "call_id": call.call_id, "correlation": corr, "ask_id": ask_id })
                    .to_string(),
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
        .env_dual("ROOT", &root.dir)
        .env_dual("DB", root.db())
        .env_dual("TRACE", root.trace_file());
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
    let stdout = out_h
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr = err_h
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
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
    let mut tty_out = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .ok()?;
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
fn check_token_budget(
    root: &Root,
    conn: &Connection,
    root_type: &str,
    event_id: Option<i64>,
) -> Result<()> {
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
        params![
            session,
            msg["role"].as_str().unwrap_or("?"),
            msg.to_string(),
            event_id
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::SandboxCfg;

    fn cfg(workdir: Option<&str>) -> SandboxCfg {
        SandboxCfg {
            workdir: workdir.map(String::from),
            ..Default::default()
        }
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
        let got = resolve_workdir(&cfg(Some(&p.display().to_string())))
            .unwrap()
            .unwrap();
        assert_eq!(got, p);
        std::fs::remove_dir_all(&p).ok();
    }

    // ───────────────────────── M3: dispatcher provider resolution ─────────────────────────

    use crate::paths::Root;
    use crate::provider::{self, Credential, Provider, Secret, Wire};
    use rusqlite::Connection;

    fn prov_root(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!(
            "el-exec-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    fn profile_with(provider: Option<&str>) -> profile::Profile {
        let mut p = profile::Profile::default();
        p.model.provider = provider.map(String::from);
        p
    }

    #[test]
    fn provider_plan_resolves_endpoint_and_literal_key() {
        let root = prov_root("apikey");
        let conn = Connection::open(root.db()).unwrap();
        provider::add(
            &root,
            &conn,
            &Provider {
                name: "deepseek".into(),
                credential: Credential::ApiKey {
                    wire: Wire::Anthropic,
                    base_url: "https://api.deepseek.com/anthropic".into(),
                    key: Secret::new("sk-live-123"),
                    headers: vec![("X-LiteLLM".into(), Secret::new("hdr-secret"))],
                },
            },
        )
        .unwrap();

        let prof = profile_with(Some("deepseek"));
        let plan = dispatcher_plan(&root, &conn, &prof, None).unwrap();
        match plan {
            DispatcherPlan::Provider(d) => {
                assert_eq!(d.wire, Wire::Anthropic);
                assert_eq!(d.base_url, "https://api.deepseek.com/anthropic");
                // The vault now stores the literal secret, not an env-var name.
                assert_eq!(d.key.expose(), "sk-live-123");
                assert_eq!(d.headers[0].0, "X-LiteLLM");
                assert_eq!(d.headers[0].1.expose(), "hdr-secret");
                // The endpoint normalization the client will apply (Anthropic wire
                // → /v1/ suffix), proving the wire drives normalization.
                assert_eq!(
                    normalize_base_url(&d.base_url, wire_adapter(d.wire)),
                    "https://api.deepseek.com/anthropic/v1/"
                );
            }
            other => panic!("expected a Provider plan, got {other:?}"),
        }
        // build_client must succeed end-to-end for an ApiKey provider.
        let (client, _) = build_client(&root, &conn, &prof).unwrap();
        assert_eq!(
            client.adapter_kind(),
            Some(genai::adapter::AdapterKind::Anthropic),
            "a named provider binds bare model names to the provider wire"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn provider_headers_ride_per_call_chat_options() {
        // The provider's extra headers must land on the PER-CALL ChatOptions
        // build_client returns (which the dispatcher threads into exec_chat) —
        // NOT only into DispatcherInjection, and NOT client-level (genai 0.6.5's
        // Anthropic/OpenAI adapters merge extra_headers only from the per-call
        // options argument, so client-level headers never reach the wire).
        let root = prov_root("hdrs");
        let conn = Connection::open(root.db()).unwrap();
        provider::add(
            &root,
            &conn,
            &Provider {
                name: "litellm".into(),
                credential: Credential::ApiKey {
                    wire: Wire::Anthropic,
                    base_url: "https://proxy.example/anthropic".into(),
                    key: Secret::new("sk-live-123"),
                    headers: vec![("X-LiteLLM-Tag".into(), Secret::new("hdr-secret"))],
                },
            },
        )
        .unwrap();

        let prof = profile_with(Some("litellm"));
        let (_client, chat_opts) = build_client(&root, &conn, &prof).unwrap();
        let opts = chat_opts.expect("provider with headers yields per-call ChatOptions");
        let headers = opts
            .extra_headers
            .expect("the header rides on the per-call ChatOptions");
        let got: std::collections::HashMap<String, String> = headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        assert_eq!(
            got.get("X-LiteLLM-Tag").map(String::as_str),
            Some("hdr-secret")
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn native_login_provider_refuses_dispatcher() {
        let root = prov_root("native");
        let conn = Connection::open(root.db()).unwrap();
        provider::add(
            &root,
            &conn,
            &Provider {
                name: "chatgpt".into(),
                credential: Credential::NativeLogin { tool: None },
            },
        )
        .unwrap();

        let prof = profile_with(Some("chatgpt"));
        // The agent must fail to start with the legible refusal — a native login
        // has no secret to feed the genai dispatcher.
        let err = dispatcher_plan(&root, &conn, &prof, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("native-login"), "legible refusal: {err}");
        assert!(build_client(&root, &conn, &prof).is_err());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn unknown_provider_name_errors_legibly() {
        let root = prov_root("missing");
        let conn = Connection::open(root.db()).unwrap();
        let prof = profile_with(Some("nope"));
        let err = dispatcher_plan(&root, &conn, &prof, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no such provider"), "{err}");
        assert!(err.contains("nope"), "{err}");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn no_provider_keeps_inline_and_default_paths() {
        let root = prov_root("inline");
        let conn = Connection::open(root.db()).unwrap();

        // No provider, no inline fields, no env → the genai default client.
        let bare = profile_with(None);
        assert!(matches!(
            dispatcher_plan(&root, &conn, &bare, None).unwrap(),
            DispatcherPlan::Default
        ));
        let (client, _) = build_client(&root, &conn, &bare).unwrap();
        assert_eq!(
            client.adapter_kind(),
            None,
            "default path keeps genai's model-name adapter inference"
        );

        // No provider, but an inline base_url → the deprecated inline path, intact.
        let mut inline = profile_with(None);
        inline.model.base_url = Some("https://api.deepseek.com/anthropic".into());
        inline.model.api_key_env = Some("DEEPSEEK_API_KEY".into());
        match dispatcher_plan(&root, &conn, &inline, None).unwrap() {
            DispatcherPlan::Inline {
                profile_url,
                api_key_env,
                ..
            } => {
                assert_eq!(
                    profile_url.as_deref(),
                    Some("https://api.deepseek.com/anthropic")
                );
                assert_eq!(api_key_env.as_deref(), Some("DEEPSEEK_API_KEY"));
            }
            other => panic!("expected Inline, got {other:?}"),
        }

        // ANTHROPIC_BASE_URL alone (threaded as env_url) also takes the inline path.
        let env_only = profile_with(None);
        assert!(matches!(
            dispatcher_plan(&root, &conn, &env_only, Some("https://x".into())).unwrap(),
            DispatcherPlan::Inline { .. }
        ));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn provider_wins_over_inline_fields() {
        let root = prov_root("wins");
        let conn = Connection::open(root.db()).unwrap();
        provider::add(
            &root,
            &conn,
            &Provider {
                name: "ds".into(),
                credential: Credential::ApiKey {
                    wire: Wire::OpenAI,
                    base_url: "https://provider.example/v1".into(),
                    key: Secret::new("sk-from-provider"),
                    headers: vec![],
                },
            },
        )
        .unwrap();
        // Both a provider AND inline fields set: the provider must win wholesale.
        let mut prof = profile_with(Some("ds"));
        prof.model.base_url = Some("https://inline.example".into());
        prof.model.api_key_env = Some("IGNORED_ENV".into());
        match dispatcher_plan(&root, &conn, &prof, Some("https://env".into())).unwrap() {
            DispatcherPlan::Provider(d) => {
                assert_eq!(d.base_url, "https://provider.example/v1");
                assert_eq!(d.key.expose(), "sk-from-provider");
            }
            other => panic!("provider must win over inline, got {other:?}"),
        }
        let (client, _) = build_client(&root, &conn, &prof).unwrap();
        assert_eq!(
            client.adapter_kind(),
            Some(genai::adapter::AdapterKind::OpenAI),
            "OpenAI-wire providers bind bare model names to OpenAI"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    // M1 (docs/handoffs/chat-rendering.md): send_message (no suspend) and
    // ask_human (suspend) are the SAME emit. This proves both verbs travel the
    // one shared path `emit_message`, landing on the same channel topic and
    // threading by correlation — the difference (deadline/default for ask,
    // none for send) is just the OutboundMessage the caller hands in, and the
    // suspend/keep-working choice lives in the handler, not the emit.
    #[test]
    fn send_and_ask_share_one_emit_path() {
        let dir = std::env::temp_dir().join(format!("el-sendask-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = Root { dir: dir.clone() };
        let conn = crate::db::open(&root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let owner = "owner";
        let topic = crate::topic::human_mailbox(owner);

        // send_message: fire-and-forget, threaded onto the turn correlation,
        // no deadline/default. The handler would NOT suspend after this.
        let send_id = emit_message(
            &root,
            &conn,
            None,
            &OutboundMessage {
                topic: topic.clone(),
                payload: json!({ "text": "heads up", "session": "s1" }),
                correlation: Some("turn-corr".to_string()),
                deadline: None,
                default_action: None,
            },
        )
        .unwrap();

        // ask_human: same path, but mints its own correlation + carries a
        // deadline/default. The handler would suspend + park pending_ask.
        let ask_id = emit_message(
            &root,
            &conn,
            None,
            &OutboundMessage {
                topic: topic.clone(),
                payload: json!({ "question": "ok?", "session": "s1" }),
                correlation: Some("ask-corr".to_string()),
                deadline: Some("2099-01-01T00:00:00.000Z".to_string()),
                default_action: Some(json!("assume yes")),
            },
        )
        .unwrap();

        // Both landed on the SAME channel (the owner's mailbox).
        let row = |id: i64| -> (String, Option<String>, Option<String>, Option<String>) {
            conn.query_row(
                "SELECT type, correlation_id, deadline, default_action FROM events WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        };
        let (s_type, s_corr, s_deadline, s_default) = row(send_id);
        let (a_type, a_corr, a_deadline, a_default) = row(ask_id);

        assert_eq!(s_type, topic, "send lands on the owner mailbox");
        assert_eq!(a_type, topic, "ask lands on the SAME owner mailbox");

        // send threads onto the turn correlation and carries no ask machinery.
        assert_eq!(s_corr.as_deref(), Some("turn-corr"));
        assert!(s_deadline.is_none(), "send is fire-and-forget: no deadline");
        assert!(s_default.is_none(), "send has no default-on-expiry");

        // ask carries its own correlation + the deadline/default it parks on.
        assert_eq!(a_corr.as_deref(), Some("ask-corr"));
        assert!(a_deadline.is_some(), "ask carries a deadline");
        assert!(a_default.is_some(), "ask carries a default-on-expiry");

        std::fs::remove_dir_all(&dir).ok();
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
    let requested_profile = payload["profile"].as_str().unwrap_or("default");
    let profile = resolve_exec_profile(root, requested_profile);
    let resume = env.get("resume").filter(|r| !r.is_null());
    // The dispatching event rides into the context document verbatim
    // (docs/context.md): stages see topic, payload, correlation — and the
    // broker-verified `sender` (docs/identity.md), so a stage can tell who the
    // kernel holds responsible for this event rather than trusting a body
    // field. Identity-bearing stages (e.g. recall) MUST key off this, never a
    // self-claimed payload field. Set from the authenticated connection; absent
    // ("unknown") on pre-sender rows or kernel-side execs without an envelope.
    let event = json!({
        "id": env["id"],
        "topic": env["type"],
        "payload": payload,
        "correlation_id": env["correlation_id"],
        "sender": env["sender"],
    });
    let opts = if let Some(r) = resume {
        let ans = match &r["payload"]["answer"] {
            Value::String(s) => s.clone(),
            Value::Null => "(no answer; deadline expired with no default)".to_string(),
            v => v.to_string(),
        };
        ExecOpts {
            session: Some(session),
            profile,
            prompt: None,
            resume: Some(ans),
            event: Some(event),
        }
    } else {
        let prompt = payload["prompt"]
            .as_str()
            .or_else(|| payload["text"].as_str());
        let Some(prompt) = prompt else {
            if !payload["answer"].is_null() {
                // An answer addressed to the agent mailbox (in/agent/<noun>)
                // shares the mailbox topic with ordinary work since v3; the
                // dispatcher's resume machinery already delivers it to the
                // suspended session by correlation, so this fresh dispatch of
                // the answer event itself is a no-op, not an error.
                eprintln!(
                    "[handle-exec] answer event; resume is correlation-driven, nothing to do"
                );
                return Ok(());
            }
            bail!("agent mailbox (in/agent/<noun>) payload needs a 'prompt' (or 'text') field");
        };
        let prompt = prompt.to_string();
        ExecOpts {
            session: Some(session),
            profile,
            prompt: Some(prompt),
            resume: None,
            event: Some(event),
        }
    };
    run(root, opts)
}

fn resolve_exec_profile(root: &Root, profile: &str) -> String {
    if profile == "helper" && !root.profile_dir("helper").join("profile.toml").exists() {
        "default".to_string()
    } else {
        profile.to_string()
    }
}
