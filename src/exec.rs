use crate::db;
use crate::events::{self, EmitOpts};
use crate::paths::Root;
use crate::profile;
use crate::render;
use crate::trace;
use anyhow::{anyhow, bail, Context, Result};
use genai::chat::{ChatMessage, ChatRequest, Tool, ToolCall, ToolResponse};
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

/// `harness exec` — run an agent turn. Chat is exec with a session ID.
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

    let system = render::render(root, &conn, &opts.profile, &session)?;
    let client = Client::default();
    let model = prof.model.model.clone();
    let tools = tool_defs();
    let root_type = match event_id {
        Some(id) => db::root_type(&conn, id).unwrap_or_else(|_| "cli".into()),
        None => "cli".into(),
    };
    let mut signal_watermark: i64 = conn.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM events WHERE type LIKE 'signal.%'",
        [],
        |r| r.get(0),
    )?;
    // Events emitted by this exec; excluded from signal preemption so an agent
    // emitting signal.pain doesn't get its own scream echoed back (feedback loop).
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
                    ..EmitOpts::new("signal.pain")
                },
            )?;
            bail!("max_turns ({}) reached for session {session}", prof.model.max_turns);
        }
        check_token_budget(root, &conn, &root_type, event_id)?;

        let chat_req = build_request(&conn, &session, &system, &tools)?;
        trace::write(root, "llm.request", &ids, json!({ "model": model, "turn": turns }));
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
            "llm.response",
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
            return Ok(());
        }

        let mut suspended_at: Option<usize> = None;
        for (i, call) in tool_calls.iter().enumerate() {
            // tool.call goes to the trace BEFORE execution: a crash mid-tool
            // must be visible as a call with no result.
            trace::write(
                root,
                "tool.call",
                &ids,
                json!({ "call_id": call.call_id, "name": call.fn_name, "args": call.fn_arguments }),
            );
            match run_tool(root, &conn, &session, event_id, in_handler, call, &mut self_emitted) {
                ToolOutcome::Output(result) => {
                    trace::write(
                        root,
                        "tool.result",
                        &ids,
                        json!({ "call_id": call.call_id, "name": call.fn_name, "result": trace::clip(&result, 2000) }),
                    );
                    store_msg(
                        &conn,
                        &session,
                        event_id,
                        &json!({
                            "role": "tool",
                            "tool_call_id": call.call_id,
                            "name": call.fn_name,
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
                    "tool.result",
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

        // Algedonic preemption: between tool batches, new signal.* events
        // (not our own) interrupt the loop as injected context.
        let sigs: Vec<(i64, String, Option<String>)> = {
            let mut stmt = conn.prepare(
                "SELECT id, type, payload FROM events WHERE type LIKE 'signal.%' AND id > ?1 ORDER BY id",
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
            trace::write(root, "signal", &ids, json!({ "injected": sigs.len() }));
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

/// Rebuild genai chat messages from the normalized transcript rows.
fn build_request(conn: &Connection, session: &str, system: &str, tools: &[Tool]) -> Result<ChatRequest> {
    let rows: Vec<String> = {
        let mut stmt =
            conn.prepare("SELECT content FROM messages WHERE session_id=?1 ORDER BY id ASC")?;
        let r = stmt
            .query_map([session], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };
    let mut msgs: Vec<ChatMessage> = Vec::new();
    for raw in rows {
        let m: Value = serde_json::from_str(&raw)?;
        match m["role"].as_str().unwrap_or("") {
            "user" => msgs.push(ChatMessage::user(m["text"].as_str().unwrap_or_default())),
            "assistant" => {
                let text = m["text"].as_str().unwrap_or_default();
                if let Some(calls) = m["tool_calls"].as_array() {
                    // Keep the assistant's interleaved commentary: replay text
                    // first, then the tool calls.
                    if !text.is_empty() {
                        msgs.push(ChatMessage::assistant(text));
                    }
                    let tcs: Vec<ToolCall> = calls
                        .iter()
                        .map(|c| ToolCall {
                            call_id: c["call_id"].as_str().unwrap_or_default().to_string(),
                            fn_name: c["fn_name"].as_str().unwrap_or_default().to_string(),
                            fn_arguments: c["fn_arguments"].clone(),
                            thought_signatures: None,
                        })
                        .collect();
                    msgs.push(ChatMessage::from(tcs));
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
                msgs.push(ChatMessage::from(ToolResponse::from_tool_call(
                    &tc,
                    m["content"].as_str().unwrap_or_default(),
                )));
            }
            _ => {}
        }
    }
    Ok(ChatRequest::new(msgs)
        .with_system(system)
        .with_tools(tools.to_vec()))
}

fn tool_defs() -> Vec<Tool> {
    vec![
        Tool::new("shell")
            .with_description(
                "Run a shell command on the host via sh -c. Working directory is the harness root. \
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
                 causality (cause_id) is threaded automatically. Use signal.* types for algedonic signals.",
            )
            .with_schema(json!({
                "type": "object",
                "properties": {
                    "type": { "type": "string", "description": "event type, e.g. demo.echo or signal.pain" },
                    "payload": { "type": "object" },
                    "priority": { "type": "integer" }
                },
                "required": ["type"]
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

fn run_tool(
    root: &Root,
    conn: &Connection,
    session: &str,
    event_id: Option<i64>,
    in_handler: bool,
    call: &ToolCall,
    self_emitted: &mut HashSet<i64>,
) -> ToolOutcome {
    let args = &call.fn_arguments;
    let err = |msg: String| ToolOutcome::Output(json!({ "error": msg }).to_string());
    match call.fn_name.as_str() {
        "shell" => {
            let Some(cmd) = args["command"].as_str() else {
                return err("shell: missing 'command'".into());
            };
            let timeout = args["timeout_secs"].as_u64().unwrap_or(120);
            ToolOutcome::Output(run_shell(root, cmd, timeout))
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
                    ..EmitOpts::new("human.ask")
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
                "tool.result",
                &ids,
                json!({ "call_id": call.call_id, "name": "ask_human", "suspended": true, "ask_id": ask_id }),
            );
            eprintln!("suspending: waiting on human (ask #{ask_id}, correlation {corr})");
            ToolOutcome::Suspend
        }
        other => err(format!("unknown tool: {other}")),
    }
}

fn run_shell(root: &Root, cmd: &str, timeout_secs: u64) -> String {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut c = Command::new("sh");
    c.arg("-c")
        .arg(cmd)
        .current_dir(&root.dir)
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
        .filter(|(pat, _)| crate::dispatcher::glob_match(pat, root_type))
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
                ..EmitOpts::new("signal.pain")
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

/// `harness handle-exec` — the two-line-script backend for exec-as-handler.
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
    let opts = if let Some(r) = resume {
        let ans = match &r["payload"]["answer"] {
            Value::String(s) => s.clone(),
            Value::Null => "(no answer; deadline expired with no default)".to_string(),
            v => v.to_string(),
        };
        ExecOpts { session: Some(session), profile, prompt: None, resume: Some(ans) }
    } else {
        let prompt = payload["prompt"]
            .as_str()
            .or_else(|| payload["text"].as_str())
            .ok_or_else(|| anyhow!("agent.exec payload needs a 'prompt' (or 'text') field"))?
            .to_string();
        ExecOpts { session: Some(session), profile, prompt: Some(prompt), resume: None }
    };
    run(root, opts)
}
