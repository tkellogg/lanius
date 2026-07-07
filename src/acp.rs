use crate::harness::Ctx;
use crate::paths::Root;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const ACP_FIDELITY: &str = "acp-live";
const ENV_ACP_ARGV: &str = "LANIUS_ACP_ARGV";
const APPROVAL_DEADLINE_SECS: u64 = 300;

#[derive(Default)]
struct ChunkBuffers {
    message_id: Option<String>,
    message_text: String,
    reasoning_id: Option<String>,
    reasoning_text: String,
}

#[derive(Clone, Debug, Default)]
struct ToolState {
    title: Option<String>,
    kind: Option<String>,
}

trait AcpSink {
    fn emit(&self, leaf: &str, body: Value);
    fn claim(&self, path: &str);
    fn record(&self, native_session_id: &str);
    fn bump_active(&self);
}

impl AcpSink for Ctx {
    fn emit(&self, leaf: &str, body: Value) {
        Ctx::emit(self, leaf, body);
    }

    fn claim(&self, path: &str) {
        Ctx::claim(self, path);
    }

    fn record(&self, native_session_id: &str) {
        Ctx::record(self, native_session_id);
    }

    fn bump_active(&self) {
        Ctx::bump_active(self);
    }
}

#[derive(Clone, Debug)]
struct ApprovalDecision {
    allow: bool,
    answer: String,
    timed_out: bool,
}

trait ApprovalRelayer {
    fn request_permission(
        &self,
        params: &Value,
        session: &str,
        owner: &str,
        sink: &dyn AcpSink,
    ) -> ApprovalDecision;
}

struct LedgerApprovalRelayer<'a> {
    root: &'a Root,
}

impl<'a> LedgerApprovalRelayer<'a> {
    fn new(root: &'a Root) -> Self {
        Self { root }
    }
}

impl ApprovalRelayer for LedgerApprovalRelayer<'_> {
    fn request_permission(
        &self,
        params: &Value,
        session: &str,
        owner: &str,
        sink: &dyn AcpSink,
    ) -> ApprovalDecision {
        emit_permission_ask(self.root, params, session, owner, sink)
    }
}

/// Run the generic ACP adapter. A4 will make this argv manifest-driven; until then
/// tests and hand-wired launches may provide `LANIUS_ACP_ARGV` as a JSON string array.
pub fn run_acp_adapter(ctx: &Ctx) -> Result<ExitStatus> {
    if ctx.mode() == crate::codeagent::Mode::Tui {
        bail!("the acp harness is headless-only; use --headless or run the agent CLI directly");
    }

    let argv = std::env::var(ENV_ACP_ARGV).with_context(|| {
        format!("missing {ENV_ACP_ARGV}; A4 will stamp the configured ACP argv")
    })?;
    let argv: Vec<String> = serde_json::from_str(&argv)
        .with_context(|| format!("parsing {ENV_ACP_ARGV} as a JSON string array"))?;
    if argv.is_empty() {
        bail!("{ENV_ACP_ARGV} must contain at least the ACP agent command");
    }

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(ctx.workdir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    ctx.scrub_provider_creds(&mut cmd);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning ACP agent {:?}", argv))?;
    let stdout = child.stdout.take().context("ACP child stdout missing")?;
    let mut stdin = child.stdin.take().context("ACP child stdin missing")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => match serde_json::from_str::<Value>(&line) {
                    Ok(v) => {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                    Err(e) => eprintln!("[acp] ignoring non-JSON stdout frame: {e}: {line}"),
                },
                Err(e) => {
                    eprintln!("[acp] stdout read failed: {e}");
                    break;
                }
            }
        }
    });

    let prompt = ctx
        .prompt()
        .map(str::to_string)
        .unwrap_or_else(|| ctx.args().join(" "));
    let relayer = LedgerApprovalRelayer::new(ctx.root());
    drive_acp_session(
        ctx,
        &relayer,
        ctx.session(),
        ctx.owner(),
        ctx.workdir(),
        &prompt,
        &rx,
        &mut stdin,
        Instant::now() + Duration::from_secs(60 * 60),
    )?;
    drop(stdin);

    let wait_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= wait_deadline {
            child.kill().ok();
            return child.wait().context("waiting for killed ACP child");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn drive_acp_session<W: Write>(
    sink: &dyn AcpSink,
    relayer: &dyn ApprovalRelayer,
    elanus_session: &str,
    owner: &str,
    workdir: &Path,
    prompt: &str,
    rx: &mpsc::Receiver<Value>,
    stdin: &mut W,
    overall_deadline: Instant,
) -> Result<()> {
    let mut next_id = 1i64;
    let initialize_id = send_req(
        stdin,
        &mut next_id,
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientInfo": { "name": "lanius", "version": env!("CARGO_PKG_VERSION") },
            "clientCapabilities": {
                "fs": { "readTextFile": false, "writeTextFile": false },
                "terminal": false
            }
        }),
    )?;

    let mut phase = Phase::Initializing {
        initialize_id,
        initialize_result: None,
    };
    let mut acp_session_id: Option<String> = None;
    let mut chunks = ChunkBuffers::default();
    let mut tools: HashMap<String, ToolState> = HashMap::new();

    loop {
        if matches!(phase, Phase::Done) {
            break;
        }
        if Instant::now() >= overall_deadline {
            bail!("ACP driver deadline reached before session/prompt completed");
        }

        let msg = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(m) => m,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("ACP agent stdout closed before session/prompt completed")
            }
        };
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let has_id = msg.get("id").is_some();

        match (method.as_deref(), has_id) {
            (Some("session/update"), false) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                handle_session_update(sink, &params, &mut chunks, &mut tools)?;
            }
            (Some("session/request_permission"), true) => {
                flush_message(sink, &mut chunks);
                flush_reasoning(sink, &mut chunks);
                let id = msg.get("id").cloned().unwrap_or(Value::Null);
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let decision = relayer.request_permission(&params, elanus_session, owner, sink);
                let result = permission_result(&params, decision.allow);
                sink.emit(
                    "approval/decision",
                    json!({
                        "ts": now_iso(),
                        "method": "session/request_permission",
                        "allow": decision.allow,
                        "answer": decision.answer,
                        "timed_out": decision.timed_out,
                        "result": result,
                        "fidelity": ACP_FIDELITY
                    }),
                );
                send_frame(
                    stdin,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                )?;
            }
            (Some(m), true) => {
                let id = msg.get("id").cloned().unwrap_or(Value::Null);
                send_frame(
                    stdin,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": format!("lanius ACP driver does not handle {m}") }
                    }),
                )?;
            }
            (Some(_m), false) => {}
            (None, true) => {
                if let Some(error) = msg.get("error") {
                    bail!("ACP request failed: {error}");
                }
                let id = msg.get("id").cloned().unwrap_or(Value::Null);
                let result = msg.get("result").cloned().unwrap_or(Value::Null);
                match &mut phase {
                    Phase::Initializing {
                        initialize_id,
                        initialize_result,
                    } if id == json!(*initialize_id) => {
                        *initialize_result = Some(result.clone());
                        let new_id = send_req(
                            stdin,
                            &mut next_id,
                            "session/new",
                            json!({
                                "cwd": workdir.display().to_string(),
                                "mcpServers": [],
                            }),
                        )?;
                        phase = Phase::CreatingSession {
                            new_id,
                            initialize_result: result,
                        };
                    }
                    Phase::CreatingSession {
                        new_id,
                        initialize_result,
                    } if id == json!(*new_id) => {
                        let session_id = result
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow!("ACP session/new response missing sessionId"))?
                            .to_string();
                        sink.record(&session_id);
                        sink.emit(
                            "session/thread",
                            json!({
                                "ts": now_iso(),
                                "native_session": session_id,
                                "sessionId": session_id,
                                "initialize": initialize_result,
                                "newSession": result,
                                "fidelity": ACP_FIDELITY
                            }),
                        );
                        sink.bump_active();
                        acp_session_id = Some(session_id.clone());
                        let prompt_id = send_req(
                            stdin,
                            &mut next_id,
                            "session/prompt",
                            json!({
                                "sessionId": session_id,
                                "prompt": [{ "type": "text", "text": prompt }],
                            }),
                        )?;
                        phase = Phase::Prompting { prompt_id };
                    }
                    Phase::Prompting { prompt_id } if id == json!(*prompt_id) => {
                        flush_message(sink, &mut chunks);
                        flush_reasoning(sink, &mut chunks);
                        sink.emit(
                            "session/idle",
                            json!({
                                "ts": now_iso(),
                                "sessionId": acp_session_id,
                                "stopReason": result.get("stopReason").cloned().unwrap_or(Value::Null),
                                "result": result,
                                "fidelity": ACP_FIDELITY
                            }),
                        );
                        sink.bump_active();
                        phase = Phase::Done;
                    }
                    _ => {}
                }
            }
            (None, false) => {}
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
enum Phase {
    Initializing {
        initialize_id: i64,
        initialize_result: Option<Value>,
    },
    CreatingSession {
        new_id: i64,
        initialize_result: Value,
    },
    Prompting {
        prompt_id: i64,
    },
    Done,
}

fn send_req<W: Write>(
    stdin: &mut W,
    next_id: &mut i64,
    method: &str,
    params: Value,
) -> Result<i64> {
    let id = *next_id;
    *next_id += 1;
    send_frame(
        stdin,
        &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
    )?;
    Ok(id)
}

fn send_frame<W: Write>(stdin: &mut W, frame: &Value) -> Result<()> {
    serde_json::to_writer(&mut *stdin, frame)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn handle_session_update(
    sink: &dyn AcpSink,
    params: &Value,
    chunks: &mut ChunkBuffers,
    tools: &mut HashMap<String, ToolState>,
) -> Result<()> {
    let update = params.get("update").unwrap_or(params);
    match update.get("sessionUpdate").and_then(Value::as_str) {
        Some("agent_message_chunk") => {
            append_chunk(sink, chunks, true, update);
        }
        Some("agent_thought_chunk") => {
            append_chunk(sink, chunks, false, update);
        }
        Some("tool_call") => {
            flush_message(sink, chunks);
            flush_reasoning(sink, chunks);
            for path in location_paths(update) {
                sink.claim(&path);
            }
            let id = update
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let kind = update
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("other")
                .to_string();
            let title = update
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("tool call")
                .to_string();
            if !id.is_empty() {
                tools.insert(
                    id,
                    ToolState {
                        title: Some(title.clone()),
                        kind: Some(kind.clone()),
                    },
                );
            }
            sink.emit(
                &format!("tool/{}/call", clean_leaf(&kind)),
                stamped(json!({
                    "toolCallId": update.get("toolCallId").cloned().unwrap_or(Value::Null),
                    "title": title,
                    "kind": kind,
                    "status": update.get("status").cloned().unwrap_or(Value::Null),
                    "content": update.get("content").cloned().unwrap_or_else(|| json!([])),
                    "locations": update.get("locations").cloned().unwrap_or_else(|| json!([])),
                    "rawInput": update.get("rawInput").cloned().unwrap_or(Value::Null),
                })),
            );
            sink.bump_active();
        }
        Some("tool_call_update") => {
            flush_message(sink, chunks);
            flush_reasoning(sink, chunks);
            for path in location_paths(update) {
                sink.claim(&path);
            }
            let id = update
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let state = tools.entry(id.to_string()).or_default();
            if let Some(kind) = update.get("kind").and_then(Value::as_str) {
                state.kind = Some(kind.to_string());
            }
            if let Some(title) = update.get("title").and_then(Value::as_str) {
                state.title = Some(title.to_string());
            }
            let kind = state.kind.as_deref().unwrap_or("other");
            let status = update.get("status").and_then(Value::as_str).unwrap_or("");
            if matches!(status, "completed" | "failed") {
                sink.emit(
                    &format!("tool/{}/result", clean_leaf(kind)),
                    stamped(json!({
                        "toolCallId": id,
                        "title": state.title.clone().unwrap_or_else(|| "tool call".to_string()),
                        "kind": kind,
                        "status": status,
                        "content": update.get("content").cloned().unwrap_or(Value::Null),
                        "locations": update.get("locations").cloned().unwrap_or(Value::Null),
                        "rawOutput": update.get("rawOutput").cloned().unwrap_or(Value::Null),
                    })),
                );
                sink.bump_active();
            }
        }
        Some("plan") => {
            flush_message(sink, chunks);
            flush_reasoning(sink, chunks);
            sink.emit("session/plan", stamped(update.clone()));
            sink.bump_active();
        }
        Some("available_commands_update")
        | Some("current_mode_update")
        | Some("config_option_update")
        | Some("session_info_update")
        | Some("usage_update") => {
            flush_message(sink, chunks);
            flush_reasoning(sink, chunks);
            let leaf = match update
                .get("sessionUpdate")
                .and_then(Value::as_str)
                .unwrap_or("")
            {
                "available_commands_update" => "session/commands",
                "current_mode_update" => "session/mode",
                "config_option_update" => "session/config",
                "session_info_update" => "session/info",
                "usage_update" => "session/usage",
                _ => "session/update",
            };
            sink.emit(leaf, stamped(update.clone()));
            sink.bump_active();
        }
        Some("user_message_chunk") | None => {}
        Some(other) => {
            sink.emit(
                "session/update",
                stamped(json!({ "unhandledSessionUpdate": other, "update": update })),
            );
        }
    }
    Ok(())
}

fn append_chunk(sink: &dyn AcpSink, chunks: &mut ChunkBuffers, message: bool, update: &Value) {
    let incoming_id = update
        .get("messageId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let text = content_text(update.get("content").unwrap_or(&Value::Null));
    if message {
        if chunks
            .message_id
            .as_ref()
            .is_some_and(|current| current != &incoming_id)
        {
            flush_message(sink, chunks);
        }
        chunks.message_id = Some(incoming_id);
        chunks.message_text.push_str(&text);
    } else {
        if chunks
            .reasoning_id
            .as_ref()
            .is_some_and(|current| current != &incoming_id)
        {
            flush_reasoning(sink, chunks);
        }
        chunks.reasoning_id = Some(incoming_id);
        chunks.reasoning_text.push_str(&text);
    }
}

fn flush_message(sink: &dyn AcpSink, chunks: &mut ChunkBuffers) {
    if chunks.message_text.is_empty() {
        chunks.message_id = None;
        return;
    }
    let body = stamped(json!({
        "messageId": chunks.message_id,
        "text": chunks.message_text,
    }));
    chunks.message_id = None;
    chunks.message_text.clear();
    sink.emit("assistant/message", body);
    sink.bump_active();
}

fn flush_reasoning(sink: &dyn AcpSink, chunks: &mut ChunkBuffers) {
    if chunks.reasoning_text.is_empty() {
        chunks.reasoning_id = None;
        return;
    }
    let body = stamped(json!({
        "messageId": chunks.reasoning_id,
        "text": chunks.reasoning_text,
    }));
    chunks.reasoning_id = None;
    chunks.reasoning_text.clear();
    sink.emit("assistant/reasoning", body);
    sink.bump_active();
}

fn content_text(content: &Value) -> String {
    if content.get("type").and_then(Value::as_str) == Some("text") {
        return content
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
    }
    content
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn location_paths(update: &Value) -> Vec<String> {
    update
        .get("locations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|loc| loc.get("path").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn permission_result(params: &Value, allow: bool) -> Value {
    let wanted = if allow { "allow_" } else { "reject_" };
    if let Some(option_id) = params
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|opt| {
            opt.get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.starts_with(wanted))
        })
        .and_then(|opt| opt.get("optionId"))
        .and_then(Value::as_str)
    {
        json!({ "outcome": { "outcome": "selected", "optionId": option_id } })
    } else {
        json!({ "outcome": { "outcome": "cancelled" } })
    }
}

fn emit_permission_ask(
    root: &Root,
    params: &Value,
    session: &str,
    owner: &str,
    sink: &dyn AcpSink,
) -> ApprovalDecision {
    let correlation = uuid::Uuid::new_v4().to_string();
    let tool = params.get("toolCall").unwrap_or(&Value::Null);
    let title = tool
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("ACP tool call");
    let kind = tool.get("kind").and_then(Value::as_str).unwrap_or("other");
    let deadline_iso = (chrono::Utc::now()
        + chrono::Duration::seconds(APPROVAL_DEADLINE_SECS as i64))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let ask_payload = json!({
        "question": format!("ACP worker {session} asks approval for a {kind}: {title}"),
        "session": session,
        "format": "decision",
        "options": ["allow", "deny"],
        "approval": {
            "method": "session/request_permission",
            "kind": kind,
            "detail": title,
            "toolCall": tool,
            "options": params.get("options").cloned().unwrap_or_else(|| json!([])),
        },
    });

    let emitted = (|| -> Result<()> {
        let conn =
            crate::db::open(root).context("opening the ledger to emit the ACP approval ask")?;
        crate::db::init_schema(&conn)?;
        crate::events::emit(
            root,
            &conn,
            crate::events::EmitOpts {
                payload: Some(ask_payload.clone()),
                correlation: Some(correlation.clone()),
                deadline: Some(deadline_iso.clone()),
                default_action: Some(json!({ "answer": "deny" })),
                sender: Some(session.to_string()),
                ..crate::events::EmitOpts::new(&crate::topic::human_mailbox(owner))
            },
        )?;
        Ok(())
    })();

    if let Err(e) = emitted {
        eprintln!("[acp] approval ask emit failed ({e:#}); denying (fail-closed)");
        sink.emit(
            "approval/decision",
            stamped(json!({
                "correlation": correlation,
                "method": "session/request_permission",
                "allow": false,
                "reason": "ask-emit-failed",
            })),
        );
        return ApprovalDecision {
            allow: false,
            answer: "deny".to_string(),
            timed_out: false,
        };
    }

    sink.emit(
        "approval/ask",
        stamped(json!({
            "correlation": correlation,
            "method": "session/request_permission",
            "kind": kind,
            "detail": title,
            "deadline": deadline_iso,
            "default": "deny",
        })),
    );

    // Copied from the codex app-server relay for A3, pending a future
    // dialect-neutral consolidation with `src/codeagent.rs`.
    match await_approval_answer(root, &correlation, APPROVAL_DEADLINE_SECS) {
        Some(answer) => {
            let allow = answer_is_allow(&answer);
            sink.emit(
                "approval/answer",
                stamped(json!({
                    "correlation": correlation,
                    "answer": answer,
                    "timed_out": false,
                })),
            );
            ApprovalDecision {
                allow,
                answer,
                timed_out: false,
            }
        }
        None => {
            sink.emit(
                "approval/answer",
                stamped(json!({
                    "correlation": correlation,
                    "answer": "(timeout:deny)",
                    "timed_out": true,
                })),
            );
            ApprovalDecision {
                allow: false,
                answer: "(timeout:deny)".to_string(),
                timed_out: true,
            }
        }
    }
}

fn extract_answer(payload: &Value) -> Option<String> {
    if let Some(a) = payload.get("answer") {
        let leaf = a.get("answer").unwrap_or(a);
        return Some(match leaf {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        });
    }
    payload
        .get("prompt")
        .or_else(|| payload.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn await_approval_answer(root: &Root, correlation: &str, deadline_secs: u64) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(deadline_secs);
    loop {
        if let Ok(conn) = crate::db::open(root) {
            let rows: Vec<String> = conn
                .prepare(
                    "SELECT payload FROM events \
                     WHERE correlation_id = ?1 AND payload IS NOT NULL ORDER BY id ASC",
                )
                .and_then(|mut stmt| {
                    let mapped = stmt
                        .query_map(rusqlite::params![correlation], |r| {
                            r.get::<_, Option<String>>(0)
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(mapped.into_iter().flatten().collect())
                })
                .unwrap_or_default();
            for payload in rows {
                let v: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
                if let Some(answer) = extract_answer(&v) {
                    return Some(answer);
                }
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(750));
    }
}

fn answer_is_allow(answer: &str) -> bool {
    matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "allow" | "allowed" | "approve" | "approved" | "yes" | "y"
    )
}

fn stamped(mut body: Value) -> Value {
    if let Value::Object(map) = &mut body {
        map.insert("ts".into(), Value::String(now_iso()));
        map.insert("fidelity".into(), Value::String(ACP_FIDELITY.to_string()));
    }
    body
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn clean_leaf(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "other".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct TestSink {
        events: Mutex<Vec<(String, Value)>>,
        claims: Mutex<Vec<String>>,
        records: Mutex<Vec<String>>,
    }

    impl AcpSink for TestSink {
        fn emit(&self, leaf: &str, body: Value) {
            self.events.lock().unwrap().push((leaf.to_string(), body));
        }

        fn claim(&self, path: &str) {
            self.claims.lock().unwrap().push(path.to_string());
        }

        fn record(&self, native_session_id: &str) {
            self.records
                .lock()
                .unwrap()
                .push(native_session_id.to_string());
        }

        fn bump_active(&self) {}
    }

    struct TestRelayer {
        allow: bool,
    }

    impl ApprovalRelayer for TestRelayer {
        fn request_permission(
            &self,
            _params: &Value,
            _session: &str,
            _owner: &str,
            sink: &dyn AcpSink,
        ) -> ApprovalDecision {
            sink.emit(
                "approval/ask",
                stamped(json!({ "correlation": "test-correlation", "default": "deny" })),
            );
            ApprovalDecision {
                allow: self.allow,
                answer: if self.allow { "allow" } else { "deny" }.to_string(),
                timed_out: false,
            }
        }
    }

    struct TimeoutRelayer;

    impl ApprovalRelayer for TimeoutRelayer {
        fn request_permission(
            &self,
            _params: &Value,
            _session: &str,
            _owner: &str,
            _sink: &dyn AcpSink,
        ) -> ApprovalDecision {
            ApprovalDecision {
                allow: false,
                answer: "(timeout:deny)".to_string(),
                timed_out: true,
            }
        }
    }

    fn drive_script(
        frames: Vec<Value>,
        relayer: &dyn ApprovalRelayer,
    ) -> (Arc<TestSink>, Vec<Value>) {
        let (tx, rx) = mpsc::channel();
        for frame in frames {
            tx.send(frame).unwrap();
        }
        drop(tx);
        let sink = Arc::new(TestSink::default());
        let mut out = Vec::<u8>::new();
        drive_acp_session(
            sink.as_ref(),
            relayer,
            "code-test",
            "owner",
            Path::new("/tmp/project"),
            "do work",
            &rx,
            &mut out,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        let sent = String::from_utf8(out).unwrap();
        let frames = sent
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect();
        (sink, frames)
    }

    fn base_initialize() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {
                    "loadSession": true,
                    "promptCapabilities": { "image": true, "audio": false, "embeddedContext": true },
                    "mcpCapabilities": { "http": true, "sse": false, "acp": false },
                    "sessionCapabilities": { "list": {}, "resume": {}, "close": {} },
                    "auth": { "logout": {} }
                },
                "authMethods": [],
                "agentInfo": { "name": "fake-acp", "version": "0.0.0" }
            }
        })
    }

    #[test]
    fn scripted_fake_agent_drives_full_turn() {
        let frames = vec![
            base_initialize(),
            json!({ "jsonrpc": "2.0", "id": 2, "result": { "sessionId": "acp-session-1" } }),
            json!({ "jsonrpc": "2.0", "method": "session/update", "params": {
                "sessionId": "acp-session-1",
                "update": { "sessionUpdate": "agent_message_chunk", "messageId": "m1",
                    "content": { "type": "text", "text": "hello " } }
            }}),
            json!({ "jsonrpc": "2.0", "method": "session/update", "params": {
                "sessionId": "acp-session-1",
                "update": { "sessionUpdate": "agent_message_chunk", "messageId": "m1",
                    "content": { "type": "text", "text": "world" } }
            }}),
            json!({ "jsonrpc": "2.0", "method": "session/update", "params": {
                "sessionId": "acp-session-1",
                "update": { "sessionUpdate": "tool_call", "toolCallId": "tc1",
                    "title": "edit src/lib.rs", "kind": "edit", "status": "in_progress",
                    "locations": [{ "path": "/tmp/project/src/lib.rs" }],
                    "rawInput": { "path": "src/lib.rs" }, "content": [] }
            }}),
            json!({ "jsonrpc": "2.0", "method": "session/update", "params": {
                "sessionId": "acp-session-1",
                "update": { "sessionUpdate": "tool_call_update", "toolCallId": "tc1",
                    "status": "completed", "rawOutput": { "ok": true }, "content": [] }
            }}),
            json!({ "jsonrpc": "2.0", "id": 3, "result": { "stopReason": "end_turn" } }),
        ];
        let (sink, sent) = drive_script(frames, &TestRelayer { allow: true });

        assert_eq!(sent[0]["method"], "initialize");
        assert_eq!(
            sent[0]["params"]["clientCapabilities"]["fs"]["readTextFile"],
            false
        );
        assert_eq!(
            sent[0]["params"]["clientCapabilities"]["fs"]["writeTextFile"],
            false
        );
        assert_eq!(sent[0]["params"]["clientCapabilities"]["terminal"], false);
        assert_eq!(sent[1]["method"], "session/new");
        assert_eq!(sent[1]["params"]["mcpServers"], json!([]));
        assert_eq!(sent[2]["method"], "session/prompt");

        let events = sink.events.lock().unwrap();
        assert!(events.iter().any(|(leaf, _)| leaf == "session/thread"));
        assert!(events.iter().any(|(leaf, body)| {
            leaf == "assistant/message" && body.get("text") == Some(&json!("hello world"))
        }));
        assert!(events.iter().any(|(leaf, _)| leaf == "tool/edit/call"));
        assert!(events.iter().any(|(leaf, _)| leaf == "tool/edit/result"));
        assert!(events.iter().any(|(leaf, _)| leaf == "session/idle"));
        assert_eq!(
            sink.records.lock().unwrap().as_slice(),
            &["acp-session-1".to_string()]
        );
        assert_eq!(
            sink.claims.lock().unwrap().as_slice(),
            &["/tmp/project/src/lib.rs".to_string()]
        );
    }

    #[test]
    fn permission_request_maps_selected_option_id() {
        let frames = vec![
            base_initialize(),
            json!({ "jsonrpc": "2.0", "id": 2, "result": { "sessionId": "acp-session-1" } }),
            json!({ "jsonrpc": "2.0", "id": 40, "method": "session/request_permission", "params": {
                "sessionId": "acp-session-1",
                "toolCall": { "toolCallId": "tc1", "title": "run tests", "kind": "execute" },
                "options": [
                    { "optionId": "allow-this", "name": "Allow once", "kind": "allow_once" },
                    { "optionId": "reject-this", "name": "Reject once", "kind": "reject_once" }
                ]
            }}),
            json!({ "jsonrpc": "2.0", "id": 3, "result": { "stopReason": "end_turn" } }),
        ];
        let (_sink, sent) = drive_script(frames, &TestRelayer { allow: true });
        let reply = sent
            .iter()
            .find(|f| f.get("id") == Some(&json!(40)))
            .unwrap();
        assert_eq!(
            reply["result"],
            json!({ "outcome": { "outcome": "selected", "optionId": "allow-this" } })
        );
    }

    #[test]
    fn permission_request_denies_fail_closed_with_reject_option() {
        let frames = vec![
            base_initialize(),
            json!({ "jsonrpc": "2.0", "id": 2, "result": { "sessionId": "acp-session-1" } }),
            json!({ "jsonrpc": "2.0", "id": 41, "method": "session/request_permission", "params": {
                "sessionId": "acp-session-1",
                "toolCall": { "toolCallId": "tc1", "title": "run tests", "kind": "execute" },
                "options": [
                    { "optionId": "allow-this", "name": "Allow once", "kind": "allow_once" },
                    { "optionId": "reject-this", "name": "Reject once", "kind": "reject_once" }
                ]
            }}),
            json!({ "jsonrpc": "2.0", "id": 3, "result": { "stopReason": "end_turn" } }),
        ];
        let (_sink, sent) = drive_script(frames, &TestRelayer { allow: false });
        let reply = sent
            .iter()
            .find(|f| f.get("id") == Some(&json!(41)))
            .unwrap();
        assert_eq!(
            reply["result"],
            json!({ "outcome": { "outcome": "selected", "optionId": "reject-this" } })
        );
    }

    #[test]
    fn permission_timeout_applies_default_deny_with_reject_option() {
        let frames = vec![
            base_initialize(),
            json!({ "jsonrpc": "2.0", "id": 2, "result": { "sessionId": "acp-session-1" } }),
            json!({ "jsonrpc": "2.0", "id": 42, "method": "session/request_permission", "params": {
                "sessionId": "acp-session-1",
                "toolCall": { "toolCallId": "tc1", "title": "run tests", "kind": "execute" },
                "options": [
                    { "optionId": "allow-this", "name": "Allow once", "kind": "allow_once" },
                    { "optionId": "reject-this", "name": "Reject once", "kind": "reject_once" }
                ]
            }}),
            json!({ "jsonrpc": "2.0", "id": 3, "result": { "stopReason": "end_turn" } }),
        ];
        let (sink, sent) = drive_script(frames, &TimeoutRelayer);
        let reply = sent
            .iter()
            .find(|f| f.get("id") == Some(&json!(42)))
            .unwrap();
        assert_eq!(
            reply["result"],
            json!({ "outcome": { "outcome": "selected", "optionId": "reject-this" } })
        );
        assert!(sink.events.lock().unwrap().iter().any(|(leaf, body)| {
            leaf == "approval/decision" && body.get("timed_out") == Some(&json!(true))
        }));
    }

    #[test]
    fn unknown_server_request_returns_method_not_found() {
        let frames = vec![
            base_initialize(),
            json!({ "jsonrpc": "2.0", "id": 2, "result": { "sessionId": "acp-session-1" } }),
            json!({ "jsonrpc": "2.0", "id": 99, "method": "fs/read_text_file", "params": {
                "sessionId": "acp-session-1", "path": "/tmp/project/src/lib.rs"
            }}),
            json!({ "jsonrpc": "2.0", "id": 3, "result": { "stopReason": "end_turn" } }),
        ];
        let (_sink, sent) = drive_script(frames, &TestRelayer { allow: true });
        let reply = sent
            .iter()
            .find(|f| f.get("id") == Some(&json!(99)))
            .unwrap();
        assert_eq!(reply["error"]["code"], -32601);
    }
}
