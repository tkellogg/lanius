//! `elanus code` — launch and observe an external coding agent.
//!
//! A coding agent (Claude Code today; Codex next) is an external actor brought
//! up from the command line (docs/actors.md): the launcher is NOT the actor, the
//! running coding session is. This module is the **one envelope, two adapters**
//! core (docs/handoffs/coding-agents.md): a shared launch + identity + record
//! path, with the tool-specific surface isolated to a thin adapter (only Claude
//! Code is wired here; Codex is the next increment).
//!
//! What this increment delivers (M0 launcher scaffolding + M1 hook→bus bridge):
//!
//! - **Per-session identity (grant-scoped).** Each launch mints a fresh elanus
//!   session id and a **grant-scoped** session token (src/codesession.rs), so
//!   everything the session publishes is stamped `sender = code-<session>` by the
//!   broker — never the owner (docs/actors.md / docs/security.md entry 16: a
//!   bridge carries its own identity) — AND the session is held to a narrow
//!   structural scope (publish only its own `obs/agent/<agent>/<session>/#`,
//!   subscribe nothing), copying the webhook daemon's grant-scoped shape rather
//!   than the full-authority fenced-secret shape. The token lives in the fenced
//!   store, so the launcher (uncaged) can place it but a caged agent cannot —
//!   the asymmetry that makes the provenance real — and even if the token leaks,
//!   it carries no authority beyond the session's own telemetry.
//!
//! - **Scoped hook config, no home pollution.** A generated CC `--settings` file
//!   in the session's run scratch routes the documented hook events through
//!   `elanus code hook <event>` → the bus. We pass `--setting-sources ''` so the
//!   user's `~/.claude` (user/project/local settings, their hooks, their
//!   CLAUDE.md auto-discovery) is NOT loaded — only the generated hooks run.
//!
//! - **The coarse, ordered record.** Session start, user message, tool pre/post
//!   (Bash/Edit/Write), and stop land as `obs/agent/<name>/<session>/...`
//!   observations with the session id and a timestamp, matching the existing
//!   `obs/agent/<name>/<sess>/tool/<name>/{call,result}` grammar (src/exec.rs).
//!
//! **Two adapters, two capture mechanisms (one envelope).** The shared envelope —
//! launch, per-session grant-scoped identity, the obs grammar, the reaper — is
//! tool-agnostic; only the *capture mechanism* differs, and that is the `Tool`
//! seam (`Tool::capture`):
//!
//! - **Claude Code — a hook bridge.** The launcher inherits the child's stdio and
//!   the child's own *hooks* (a generated `--settings` config) call
//!   `elanus code hook <Event>`, which publishes. The launcher parses nothing.
//! - **Codex — a stdout stream.** Codex 0.141's hooks are plugin/managed-config
//!   based and a dead end for this (Appendix B), so the Codex adapter does NOT use
//!   hooks at all: it runs `codex exec --json`, which prints a JSONL event stream
//!   to stdout. The launcher **pipes the child's stdout, reads it line-by-line as
//!   JSONL, maps each event, and publishes the obs record itself** (in-process,
//!   authenticating as the session principal — the same scoped-token identity).
//!   No `elanus code hook` bridge, no hooks.json, no `~/.codex` pollution at all.
//!
//! **Sandbox stance for this increment (recorded in the handoff Log).** We do NOT
//! bypass Claude Code's own sandbox onto today's elanus cage. Today the cage is a
//! write-only fence (reads/network open) and is built for one-shot captured
//! `sh -c` calls, not an interactive long-lived TUI with inherited stdio
//! (src/sandbox.rs). Bypassing the tool's sandbox onto that would be a containment
//! regression (M0's read/egress acceptance criteria need the complete cage that
//! docs/sandbox.md promotes to the end state but which is not built yet). So for
//! now the tool keeps its OWN sandbox active (reads/network stay contained) and
//! elanus owns the workdir + observation + identity. The single complete cage
//! (write + read + egress + the read camera) is a separate core prerequisite; the
//! tool-sandbox bypass + posture reconstruction is a LATER milestone gated on it.

use crate::buscli;
use crate::codesession;
use crate::paths::Root;
use crate::topic;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead as _, Read as _};
use std::path::Path;

/// Env vars the launcher sets for the child coding-agent process tree, read back
/// by `elanus code hook` so each hook event publishes as the session principal.
const ENV_SESSION: &str = "ELANUS_CODE_SESSION";
const ENV_AGENT: &str = "ELANUS_CODE_AGENT";

/// The supported adapters: Claude Code (hook bridge) and Codex (`exec --json`
/// stdout stream). They share the envelope; only the capture mechanism differs.
#[derive(Clone, Copy)]
enum Tool {
    ClaudeCode,
    Codex,
}

/// How the launcher captures a session's activity — the per-tool seam.
enum Capture {
    /// The child's own hooks call `elanus code hook` (Claude Code): the launcher
    /// inherits stdio and parses nothing.
    HookBridge,
    /// The launcher pipes the child's stdout and parses its JSONL event stream
    /// in-process (Codex `exec --json`): no hooks, no home pollution.
    StreamJson,
}

impl Tool {
    fn parse(s: &str) -> Result<Tool> {
        match s {
            "claude" | "claude-code" | "cc" => Ok(Tool::ClaudeCode),
            "codex" => Ok(Tool::Codex),
            other => bail!("unknown coding tool {other:?} (supported: claude, codex)"),
        }
    }
    /// The agent noun this tool's sessions publish under: obs/agent/<noun>/...
    fn agent_noun(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "claude-code",
            Tool::Codex => "codex",
        }
    }
    /// Recover the adapter from the agent noun the launcher recorded in the
    /// session env — so the hook bridge routes event-mapping through the right
    /// adapter without re-parsing the tool name. None for an unknown noun (a
    /// future adapter whose launcher set a noun this binary doesn't know).
    fn from_agent_noun(noun: &str) -> Option<Tool> {
        match noun {
            "claude-code" => Some(Tool::ClaudeCode),
            "codex" => Some(Tool::Codex),
            _ => None,
        }
    }
    /// The real binary to launch.
    fn binary(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "claude",
            Tool::Codex => "codex",
        }
    }
    /// How the launcher captures this adapter's activity (the capture seam).
    fn capture(self) -> Capture {
        match self {
            Tool::ClaudeCode => Capture::HookBridge,
            // Codex 0.141 hooks are a plugin/managed-config dead end; capture the
            // `codex exec --json` stdout stream in-process instead (Appendix B).
            Tool::Codex => Capture::StreamJson,
        }
    }
    /// The generated tool config that routes this adapter's hook events through
    /// `elanus code hook <Event>` to the bus. Only the hook-bridge adapter
    /// (Claude Code) generates one; the stream-parse adapter (Codex) does not use
    /// hooks at all, so it has no settings (and writes nothing to the tool home).
    fn settings(self, self_exe: &Path, root: &Root) -> Option<Value> {
        match self {
            Tool::ClaudeCode => Some(claude_settings(self_exe, root)),
            Tool::Codex => None,
        }
    }
    /// Map one of this adapter's hook events + its payload to an obs/ topic leaf
    /// and a trimmed body. Adapter-specific (the hook event names and payload
    /// shapes differ per tool). Only the hook-bridge adapter routes through here;
    /// Codex maps its own JSONL stream events directly in the launcher.
    fn map_event(self, event: &str, payload: &Value) -> (String, Value) {
        match self {
            Tool::ClaudeCode => claude_map_event(event, payload),
            // Codex never reaches the hook bridge (no hooks); file generically if
            // it somehow does, so nothing is dropped.
            Tool::Codex => generic_event(event, payload),
        }
    }
}

/// `elanus code <tool> [args...]` — launch the real coding agent, observed.
pub fn launch(root: &Root, tool: &str, args: &[String]) -> Result<()> {
    // Reap any session tokens a prior SIGKILL'd launcher leaked, before anything
    // else — a crash must never leave a usable credential lying around
    // (docs/security.md). Done first (even before tool parsing) so a launch is
    // an opportunity to heal orphans regardless of how it turns out. Daemon boot
    // does the same sweep; doing it here too means a launcher heals orphans even
    // against a never-restarted daemon.
    for orphan in codesession::reap_orphans(root) {
        eprintln!("[code] reaped orphaned session credential {orphan}");
    }

    let tool = Tool::parse(tool)?;
    let session = format!("code-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let agent = tool.agent_noun().to_string();

    // Per-session identity: a GRANT-SCOPED session token (NOT a full-authority
    // fenced secret — docs/security.md entry 16). The launcher is uncaged (the
    // human ran it), so it can place the token in the fenced store; that is what
    // lets the session's hook bridge authenticate as ITSELF and the broker stamp
    // the session — not the owner — as the sender, while holding it to its own
    // obs subtree. We record this launcher's pid as the token owner so the reaper
    // can distinguish a live session from a SIGKILL orphan.
    let principal = session.clone();
    let token = codesession::mint(root, &principal, &agent, std::process::id() as i32)
        .with_context(|| format!("minting the session credential for {principal}"))?;
    let bus_token = token.secret.clone();

    // The session's run scratch — for CC, the generated hook config lives here;
    // for Codex (no hooks) it's still created for symmetry and is empty. Never
    // ~/.claude / ~/.codex.
    let scratch = root.run_dir().join(&session);
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("creating run scratch {}", scratch.display()))?;
    let settings_path = scratch.join("settings.json");

    let self_exe = std::env::current_exe().context("locating the elanus binary for hook commands")?;
    let result = (|| -> Result<std::process::ExitStatus> {
        // Session start (the first ordered record): timestamp + the resolved
        // workdir, so the bus shows when and where the session began. Emitted by
        // the launcher itself for both adapters, before the child runs.
        let workdir = std::env::current_dir().unwrap_or_else(|_| root.dir.clone());
        publish_obs(
            root,
            &principal,
            &bus_token,
            &obs_topic(&agent, &session, "session/start"),
            json!({
                "ts": now_iso(),
                "tool": tool.binary(),
                "workdir": workdir.display().to_string(),
                "args": args,
            }),
        );

        match tool.capture() {
            // ── Claude Code: hook bridge ──────────────────────────────────────
            // The child's own generated hooks call `elanus code hook`; the
            // launcher inherits stdio and parses nothing.
            Capture::HookBridge => {
                let settings = tool
                    .settings(&self_exe, root)
                    .expect("hook-bridge adapter generates settings");
                std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)
                    .with_context(|| format!("writing {}", settings_path.display()))?;

                // Launch the real binary with the generated, isolated config. The
                // TUI gets inherited stdio so it is a normal, fully usable
                // session. `--setting-sources ''` loads NO user/project/local
                // settings (the user's ~/.claude hooks/CLAUDE.md are untouched);
                // `--settings <file>` loads only our generated hooks (Appendix A).
                let mut cmd = std::process::Command::new(tool.binary());
                cmd.arg("--settings")
                    .arg(&settings_path)
                    .arg("--setting-sources")
                    .arg("");
                cmd.args(args);
                // The session's own identity, carried to the hook bridge children
                // CC spawns. ELANUS_PACKAGE + ELANUS_BUS_TOKEN are what
                // `elanus bus pub` authenticates with (src/buscli.rs);
                // ELANUS_CODE_* tell the bridge which session/agent to file under.
                cmd.env("ELANUS_PACKAGE", &principal)
                    .env("ELANUS_BUS_TOKEN", &bus_token)
                    .env(ENV_SESSION, &session)
                    .env(ENV_AGENT, &agent)
                    .env("ELANUS_ROOT", &root.dir);
                eprintln!("[code] launching {} as session {session}", tool.binary());
                cmd.status().with_context(|| {
                    format!("launching {} (is it installed and on PATH?)", tool.binary())
                })
            }
            // ── Codex: stdout JSONL stream ────────────────────────────────────
            // No hooks. Run `codex exec --json`, pipe stdout, and parse+publish
            // each event in-process as the session principal.
            Capture::StreamJson => run_codex_capture(
                root, &principal, &bus_token, &agent, &session, &workdir, args,
            ),
        }
    })();

    // Stop (the last ordered record): always emitted, even on a launch error,
    // so the bus shows the session ended and with what code.
    let exit_code = result.as_ref().ok().and_then(|s| s.code());
    publish_obs(
        root,
        &principal,
        &bus_token,
        &obs_topic(&agent, &session, "session/stop"),
        json!({ "ts": now_iso(), "exit_code": exit_code }),
    );

    // No home-state pollution and no lingering credential: drop the generated
    // config and retire the session's scoped token (best-effort; a SIGKILL leaves
    // it, but it is reaped at the next launcher/daemon boot, and even unreaped it
    // can only ever publish this dead session's own obs subtree — never the
    // owner, work, or another agent).
    let _ = std::fs::remove_dir_all(&scratch);
    codesession::retire(root, &principal);

    let status = result?;
    if !status.success() {
        // Propagate the tool's exit so a script driving the launcher sees it.
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

/// Run Codex non-interactively and capture its JSONL event stream, publishing
/// each mapped event as an obs record (in-process, as the session principal —
/// the Codex capture path; see the module header).
///
/// `codex exec --json --skip-git-repo-check [args…]`, cwd = the workdir, keeping
/// the user's real `CODEX_HOME` so auth stays intact and nothing is written to
/// `~/.codex`. We do NOT pass `--dangerously-bypass-approvals-and-sandbox`: Codex
/// keeps its OWN sandbox active (the complete elanus cage is the deferred
/// prerequisite, recorded in the handoff Log), exactly as the CC adapter keeps
/// CC's sandbox. The child gets empty stdin (the prompt comes from the user args,
/// not stdin) so it never blocks reading stdin. stderr is inherited so the human
/// still sees Codex's own progress/errors.
fn run_codex_capture(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    workdir: &Path,
    args: &[String],
) -> Result<std::process::ExitStatus> {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new("codex");
    cmd.arg("exec").arg("--json").arg("--skip-git-repo-check");
    cmd.args(args);
    // Empty stdin (prompt is in args), piped stdout (we parse it), inherited
    // stderr (the human sees Codex's own output). We keep the real CODEX_HOME —
    // setting it to a scratch would drop the user's auth.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    eprintln!("[code] launching codex exec --json as session {session}");

    let mut child = cmd
        .spawn()
        .context("launching codex (is it installed and on PATH?)")?;

    // On a fresh launch, `thread.started` carries codex's native thread id —
    // persist the durable record (with this workdir) the moment we see it so the
    // session is resumable after the launcher exits.
    capture_codex_stream(
        root, principal, bus_token, agent, session, &mut child, Some(workdir),
    );

    child
        .wait()
        .context("waiting for codex exec to finish")
}

/// Read a codex child's `--json` stdout line-by-line, mapping each JSONL event to
/// an obs record and publishing it as the session principal. Shared by launch and
/// resume (the SAME obs grammar lands under the SAME elanus session both times).
/// When `record_workdir` is `Some`, a `thread.started` event also persists/refreshes
/// the durable `code_sessions` record (launch path, carrying the workdir to store);
/// resume already has a record, so it passes `None`. A malformed line files
/// generically (nothing dropped); a read error stops the loop but never aborts.
fn capture_codex_stream(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    child: &mut std::process::Child,
    record_workdir: Option<&Path>,
) {
    let Some(out) = child.stdout.take() else {
        return;
    };
    let reader = std::io::BufReader::new(out);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                // A non-JSON line (Codex shouldn't emit one under --json, but
                // be defensive): record it generically rather than drop it.
                let (leaf, body) = generic_event("codex_nonjson_line", &Value::Null);
                publish_obs(root, principal, bus_token, &obs_topic(agent, session, &leaf), body);
                continue;
            }
        };
        // The DURABLE session record (M2-A): codex announces its own native
        // resumable session id via `thread.started` → `thread_id`. Persist the
        // record (no secret) the moment we see it, so the session is resumable
        // even after this launcher exits. Best-effort: a record-write failure
        // never breaks the live session (it just means it can't be resumed).
        if let Some(workdir) = record_workdir {
            if event.get("type").and_then(Value::as_str) == Some("thread.started") {
                if let Some(thread_id) = event.get("thread_id").and_then(Value::as_str) {
                    let rec = codesession::SessionRecord {
                        elanus_session: session.to_string(),
                        native_session: thread_id.to_string(),
                        tool: "codex".to_string(),
                        agent_noun: agent.to_string(),
                        workdir: workdir.display().to_string(),
                    };
                    if let Err(e) = codesession::upsert_record(root, &rec) {
                        eprintln!("[code] recording codex session (continuing): {e:#}");
                    }
                }
            }
        }
        if let Some((leaf, body)) = codex_map_event(&event) {
            publish_obs(root, principal, bus_token, &obs_topic(agent, session, &leaf), body);
        }
    }
}

/// Map one Codex `exec --json` stream event to an obs/ topic leaf and a trimmed
/// body, matching the exec.rs grammar (`tool/<name>/{call,result}`, session/turn
/// leaves). Returns `None` for events we deliberately drop (a redundant
/// thread-level `session/started` and bare turn markers). The event types and
/// item.type strings were confirmed against codex 0.141.0
/// (`codex exec --json`): top-level `thread.started`, `turn.started`,
/// `item.started`, `item.updated`, `item.completed`, `turn.completed`,
/// `turn.failed`, `error`; item types `agent_message`, `reasoning`,
/// `command_execution`, `file_change`, `mcp_tool_call`, `web_search`,
/// `todo_list`. Anything unmodeled still lands via `generic_event`.
fn codex_map_event(event: &Value) -> Option<(String, Value)> {
    let ts = now_iso();
    let etype = event.get("type").and_then(Value::as_str).unwrap_or("");
    match etype {
        // The launcher already emitted its own session/start at launch (workdir +
        // args). thread.started carries Codex's own thread id: record it as a
        // distinct leaf (NOT a second session/start) so the thread id is on the
        // bus without a confusing double session-start.
        "thread.started" => Some((
            "session/thread".into(),
            json!({
                "ts": ts,
                "codex_thread": event.get("thread_id").cloned().unwrap_or(Value::Null),
            }),
        )),
        // Bare turn markers: skip turn.started (no payload); turn.completed
        // carries the token usage (a cost signal) and lands as session/idle.
        "turn.started" => None,
        "turn.completed" => {
            let usage = event.get("usage").cloned().unwrap_or(Value::Null);
            Some((
                "session/idle".into(),
                json!({ "ts": ts, "event": "turn.completed", "usage": usage }),
            ))
        }
        "turn.failed" => Some((
            "session/idle".into(),
            json!({
                "ts": ts,
                "event": "turn.failed",
                "error": clip_value(event.get("error"), 4000),
            }),
        )),
        // A top-level error event (e.g. a stream/usage-limit error).
        "error" => Some((
            "session/idle".into(),
            json!({
                "ts": ts,
                "event": "error",
                "error": clip_value(event.get("message").or_else(|| event.get("error")), 4000),
            }),
        )),
        // Item lifecycle: only `item.completed` carries the settled item. We file
        // command/mcp calls' *result* on completed; the `item.started` for a
        // command is its *call* (so a tool shows as call→result like CC).
        "item.started" => codex_map_item(event.get("item")?, /*completed=*/ false, &ts),
        "item.completed" => codex_map_item(event.get("item")?, /*completed=*/ true, &ts),
        // item.updated is a streaming partial — skip (the completed item carries
        // the settled state; updates would be noisy duplicates).
        "item.updated" => None,
        // Anything else still lands, tagged by its event type, so nothing is
        // silently dropped.
        other => {
            let (leaf, mut body) = generic_event(other, event);
            if let Value::Object(m) = &mut body {
                m.insert("codex_event".into(), json!(other));
            }
            Some((leaf, body))
        }
    }
}

/// Map one settled Codex thread item (the `item` object of an `item.started` /
/// `item.completed` event) to an obs leaf + body. `completed` distinguishes a
/// command's call (started) from its result (completed). Item types confirmed
/// against codex 0.141.0; an unmodeled item type files generically.
fn codex_map_item(item: &Value, completed: bool, ts: &str) -> Option<(String, Value)> {
    let itype = item.get("type").and_then(Value::as_str).unwrap_or("");
    let item_id = item.get("id").cloned().unwrap_or(Value::Null);
    match itype {
        // The assistant's message to the user.
        "agent_message" => {
            if !completed {
                return None; // the text settles on completed
            }
            Some((
                "assistant/message".into(),
                json!({
                    "ts": ts,
                    "item_id": item_id,
                    "text": clip_opt(item.get("text"), 4000),
                }),
            ))
        }
        // The model's reasoning trace (when summaries are emitted).
        "reasoning" => {
            if !completed {
                return None;
            }
            Some((
                "assistant/reasoning".into(),
                json!({
                    "ts": ts,
                    "item_id": item_id,
                    "text": clip_opt(item.get("text"), 4000),
                }),
            ))
        }
        // A shell command Codex ran. started → tool/<name>/call,
        // completed → tool/<name>/result (carrying output + exit code), so it
        // reads like CC's Bash pre/post pair.
        "command_execution" => {
            let leaf = if completed {
                "tool/command_execution/result"
            } else {
                "tool/command_execution/call"
            };
            let mut body = json!({
                "ts": ts,
                "item_id": item_id,
                "tool": "command_execution",
                "command": clip_value(item.get("command"), 2000),
            });
            if let Value::Object(m) = &mut body {
                if completed {
                    m.insert("failed".into(), json!(!command_succeeded(item)));
                    m.insert(
                        "exit_code".into(),
                        item.get("exit_code").cloned().unwrap_or(Value::Null),
                    );
                    m.insert(
                        "output".into(),
                        clip_value(item.get("aggregated_output"), 4000),
                    );
                }
                m.insert(
                    "status".into(),
                    item.get("status").cloned().unwrap_or(Value::Null),
                );
            }
            Some((leaf.into(), body))
        }
        // An edit/write to one or more files (apply_patch). file_change settles on
        // completed; file it as a file-write leaf carrying the changed paths.
        "file_change" => {
            if !completed {
                return None;
            }
            Some((
                "file/write".into(),
                json!({
                    "ts": ts,
                    "item_id": item_id,
                    "changes": clip_value(item.get("changes"), 4000),
                    "status": item.get("status").cloned().unwrap_or(Value::Null),
                }),
            ))
        }
        // An MCP tool call. started → call, completed → result.
        "mcp_tool_call" => {
            let name = item
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("mcp_tool");
            let leaf = if completed {
                format!("tool/{}/result", topic::encode_segment(name))
            } else {
                format!("tool/{}/call", topic::encode_segment(name))
            };
            let mut body = json!({
                "ts": ts,
                "item_id": item_id,
                "tool": name,
                "server": item.get("server").cloned().unwrap_or(Value::Null),
                "arguments": clip_value(item.get("arguments"), 2000),
            });
            if completed {
                if let Value::Object(m) = &mut body {
                    m.insert("result".into(), clip_value(item.get("result"), 4000));
                    m.insert(
                        "status".into(),
                        item.get("status").cloned().unwrap_or(Value::Null),
                    );
                }
            }
            Some((leaf, body))
        }
        // A web search the model ran.
        "web_search" => {
            if !completed {
                return None;
            }
            Some((
                "tool/web_search/result".into(),
                json!({
                    "ts": ts,
                    "item_id": item_id,
                    "tool": "web_search",
                    "query": clip_value(item.get("query"), 1000),
                }),
            ))
        }
        // A todo/plan list update.
        "todo_list" => {
            if !completed {
                return None;
            }
            Some((
                "assistant/todo".into(),
                json!({
                    "ts": ts,
                    "item_id": item_id,
                    "items": clip_value(item.get("items"), 4000),
                }),
            ))
        }
        // Any item type this binary doesn't model: file it generically (tagged by
        // item type) so nothing is dropped. Only on completed to avoid a noisy
        // started/completed pair for items we don't understand.
        other => {
            if !completed {
                return None;
            }
            Some((
                format!("item/{}", topic::encode_segment(other)),
                json!({ "ts": ts, "item_id": item_id, "item_type": other }),
            ))
        }
    }
}

/// A `command_execution` item succeeded iff it completed with exit code 0.
fn command_succeeded(item: &Value) -> bool {
    item.get("exit_code").and_then(Value::as_i64) == Some(0)
}

// ── The resume primitive (M2-A) ──────────────────────────────────────────────
//
// `elanus code resume <elanus_session> "<message>"` continues a recorded session.
// It is the foundation of inbound delivery (M2-B): a session has a DURABLE record
// (no secret) but no idle token; resume mints a FRESH scoped token, runs the
// tool's native resume in the recorded workdir capturing output into the SAME obs
// tree under the SAME elanus session, publishes the result, retires the token, and
// bumps last_active. The token is emit-only on resume too (no read/subscribe grant
// — that is M3's interactive-pull). M2-B (the daemon driving resume off a session
// mailbox message) is deferred: the DAEMON has the authority to read the mailbox
// and call this; the session itself never gains read authority.

/// Build the native resume command (program + args) for a recorded session and a
/// message. Pure and unit-testable — no process spawn, no env. The resume runs in
/// the record's `workdir` (set by the caller via `Command::current_dir`):
/// - **codex:** `codex exec resume <thread_id> --json --skip-git-repo-check "<msg>"`
///   — confirmed against codex-cli 0.141.0 (`codex exec resume [SESSION_ID]
///   [PROMPT]`, with `--json` JSONL stdout and `--skip-git-repo-check`). Note
///   `codex exec resume` has NO `--cd`, so the workdir is set as the child cwd.
/// - **claude:** `claude -p --resume <session_id> --output-format stream-json
///   --verbose "<msg>"` — headless print, resuming the recorded native session id,
///   capturing the JSONL result stream (the generated hooks are NOT reloaded on a
///   bare `-p --resume`, so resume parses the stream like codex rather than relying
///   on hooks). Confirmed flags against Claude Code 2.1.183.
fn resume_command(rec: &codesession::SessionRecord, message: &str) -> (String, Vec<String>) {
    match rec.tool.as_str() {
        "codex" => (
            "codex".to_string(),
            vec![
                "exec".to_string(),
                "resume".to_string(),
                rec.native_session.clone(),
                "--json".to_string(),
                "--skip-git-repo-check".to_string(),
                message.to_string(),
            ],
        ),
        // Default to the claude shape for "claude" (and any CC-noun record).
        _ => (
            "claude".to_string(),
            vec![
                "-p".to_string(),
                "--resume".to_string(),
                rec.native_session.clone(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--verbose".to_string(),
                message.to_string(),
            ],
        ),
    }
}

/// `elanus code resume <elanus_session> "<message>"` — continue a recorded coding
/// session with a fresh, emit-only scoped token, capturing the result under the
/// same elanus session. Returns an error if there is no resumable record.
pub fn resume(root: &Root, elanus_session: &str, message: &str) -> Result<()> {
    use std::process::{Command, Stdio};

    // Heal any orphaned credentials a prior crash leaked, same as launch.
    for orphan in codesession::reap_orphans(root) {
        eprintln!("[code] reaped orphaned session credential {orphan}");
    }

    let rec = codesession::read_record(root, elanus_session)
        .context("reading the coding-session record")?
        .with_context(|| {
            format!(
                "no resumable coding session {elanus_session:?} \
                 (never launched, or its native session id was never observed)"
            )
        })?;

    // Mint a FRESH scoped token for this resume run, with the SAME deterministic
    // principal/scope derived from the session name — exactly as a launch does.
    // An idle session has no token; this one lives only for the resume and is
    // retired at the end (reaped on crash). It is emit-only: no read/subscribe
    // grant (M3's interactive-pull is deferred), so resume cannot read the bus.
    let principal = rec.elanus_session.clone();
    let token = codesession::mint(root, &principal, &rec.agent_noun, std::process::id() as i32)
        .with_context(|| format!("minting the resume credential for {principal}"))?;
    let bus_token = token.secret.clone();
    let agent = rec.agent_noun.clone();
    let session = rec.elanus_session.clone();
    let workdir = std::path::PathBuf::from(&rec.workdir);

    let (program, cmd_args) = resume_command(&rec, message);

    // A resume marker under the SAME elanus session, so the bus shows the session
    // continued and with what message.
    publish_obs(
        root,
        &principal,
        &bus_token,
        &obs_topic(&agent, &session, "session/resume"),
        json!({
            "ts": now_iso(),
            "tool": rec.tool,
            "native_session": rec.native_session,
            "workdir": rec.workdir,
            "message": clip(message, 4000),
        }),
    );

    let result = (|| -> Result<std::process::ExitStatus> {
        let mut cmd = Command::new(&program);
        cmd.args(&cmd_args);
        // Run in the recorded workdir so the native session continues against the
        // same files. Empty stdin (the message is an arg), piped stdout (we parse
        // the JSONL result), inherited stderr (the human sees tool progress). We
        // keep the real CODEX_HOME / ~/.claude so auth stays intact.
        cmd.current_dir(&workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        eprintln!("[code] resuming {} session {session} ({})", rec.tool, rec.native_session);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("launching {program} resume (is it installed and on PATH?)"))?;

        match rec.tool.as_str() {
            // Both adapters' resume emit a JSONL stream on stdout. Codex's `exec
            // resume --json` is identical to the launch stream (thread.started for
            // the resumed thread, item.*; record_thread=false — we already have a
            // record). Claude's `-p --output-format stream-json` is a DIFFERENT
            // JSONL grammar; map it via the CC stream mapper.
            "codex" => {
                // record_workdir = None: the record already exists (we read it).
                capture_codex_stream(
                    root, &principal, &bus_token, &agent, &session, &mut child, None,
                );
            }
            _ => {
                capture_claude_stream(root, &principal, &bus_token, &agent, &session, &mut child);
            }
        }
        child.wait().context("waiting for the resume to finish")
    })();

    // Retire the per-resume token — no idle credential is left behind (a SIGKILL
    // would leak it, but it is reaped at the next launcher/daemon boot, and even
    // unreaped it can only publish this dead session's own obs subtree). Bump
    // last_active so the record reflects the resume.
    codesession::retire(root, &principal);
    let _ = codesession::touch_record(root, &session);

    let status = result?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

/// Read a Claude Code `-p --output-format stream-json` child's stdout line-by-line,
/// mapping each JSONL message to an obs record under the resumed elanus session.
/// Claude's print stream is a different grammar from codex's: top-level objects
/// with a `type` of `system` (init), `assistant`/`user` (message turns carrying a
/// nested `message` with `content` blocks: `text`, `tool_use`, `tool_result`), and
/// `result` (the final settle, carrying `result` text + `session_id` + usage). We
/// map the load-bearing ones onto the existing obs grammar so a resumed turn reads
/// like a launched one; anything unmodeled lands generically (nothing dropped).
fn capture_claude_stream(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    child: &mut std::process::Child,
) {
    let Some(out) = child.stdout.take() else {
        return;
    };
    let reader = std::io::BufReader::new(out);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // non-JSON line on the print stream: skip
        };
        if let Some((leaf, body)) = claude_stream_map(&event) {
            publish_obs(root, principal, bus_token, &obs_topic(agent, session, &leaf), body);
        }
    }
}

/// Map one Claude Code `--output-format stream-json` top-level message to an obs
/// leaf + body. Returns None for messages we deliberately drop. Confirmed against
/// Claude Code 2.1.183's print stream.
fn claude_stream_map(event: &Value) -> Option<(String, Value)> {
    let ts = now_iso();
    let etype = event.get("type").and_then(Value::as_str).unwrap_or("");
    let subtype = event.get("subtype").and_then(Value::as_str).unwrap_or("");
    match etype {
        // Only the `init` system message (model/tools/cwd) records the resumed
        // session id as session/started — ONCE. Any other `system` subtype (and a
        // resume replays prior-turn system frames) is dropped, so a long history
        // does not flood the bus with duplicate starts. Confirmed against CC
        // 2.1.183: a clean print/resume emits exactly one `system/init`.
        "system" if subtype == "init" => Some((
            "session/started".into(),
            json!({
                "ts": ts,
                "cc_session": event.get("session_id").cloned().unwrap_or(Value::Null),
                "subtype": "init",
            }),
        )),
        "system" => None,
        // Per-turn rate-limit telemetry — not a session happening; drop it.
        "rate_limit_event" => None,
        // An assistant/user turn: the nested message carries content blocks. We
        // file tool_use as a tool call, tool_result as a tool result, and text as
        // an assistant message, matching the obs grammar.
        "assistant" | "user" => claude_stream_message(event, &ts),
        // The final settle: the model's answer text + usage + the session id.
        "result" => Some((
            "session/idle".into(),
            json!({
                "ts": ts,
                "event": "result",
                "cc_session": event.get("session_id").cloned().unwrap_or(Value::Null),
                "result": clip_value(event.get("result"), 4000),
                "usage": event.get("usage").cloned().unwrap_or(Value::Null),
                "is_error": event.get("is_error").cloned().unwrap_or(Value::Null),
            }),
        )),
        // Anything else (stream_event partials, etc.) lands generically.
        other => {
            let (leaf, mut body) = generic_event(other, event);
            if let Value::Object(m) = &mut body {
                m.insert("cc_stream_event".into(), json!(other));
            }
            Some((leaf, body))
        }
    }
}

/// Map the content blocks of a Claude print-stream `assistant`/`user` message to a
/// single obs record (the first load-bearing block). A turn typically carries one
/// salient block: text (assistant message), tool_use (a tool call), or tool_result
/// (a tool result). We file that block; finer block-by-block fan-out is M3's job.
fn claude_stream_message(event: &Value, ts: &str) -> Option<(String, Value)> {
    let cc_session = event.get("session_id").cloned().unwrap_or(Value::Null);
    let content = event.get("message")?.get("content")?.as_array()?;
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                return Some((
                    "assistant/message".into(),
                    json!({ "ts": ts, "cc_session": cc_session, "text": clip_opt(block.get("text"), 4000) }),
                ));
            }
            Some("tool_use") => {
                let tool = block.get("name").and_then(Value::as_str).unwrap_or("unknown");
                return Some((
                    format!("tool/{}/call", topic::encode_segment(tool)),
                    json!({ "ts": ts, "cc_session": cc_session, "tool": tool, "input": clip_value(block.get("input"), 4000) }),
                ));
            }
            Some("tool_result") => {
                return Some((
                    "tool/result".into(),
                    json!({ "ts": ts, "cc_session": cc_session, "content": clip_value(block.get("content"), 4000) }),
                ));
            }
            _ => continue,
        }
    }
    None
}

/// `elanus code hook <event>` — the bridge. Reads the Claude Code hook JSON
/// payload on stdin and publishes one ordered observation to the bus as the
/// session principal. Always exits 0 (and prints nothing on stdout): a hook that
/// fails or emits stray output must never break or alter the coding session.
pub fn hook(root: &Root, event: &str) -> Result<()> {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let payload: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);

    let (Ok(principal), Ok(token), Ok(session), Ok(agent)) = (
        std::env::var("ELANUS_PACKAGE"),
        std::env::var("ELANUS_BUS_TOKEN"),
        std::env::var(ENV_SESSION),
        std::env::var(ENV_AGENT),
    ) else {
        // Outside a launched session (no identity in the env): nothing to file,
        // and we must not fail the coding session. Stay quiet.
        return Ok(());
    };

    // The DURABLE session record (M2-A): Claude Code carries its own native
    // resumable session id in every hook payload (`session_id`). On SessionStart —
    // the first hook of a run — persist the record (elanus session ↔ CC session_id
    // ↔ workdir), so the session is resumable (`claude -p --resume <session_id>`)
    // even after the launcher exits. The record carries no secret. Best-effort: a
    // failure here must never break the hook or the coding session.
    if matches!(event, "SessionStart" | "Setup") {
        if let Some(native) = payload.get("session_id").and_then(Value::as_str) {
            let workdir = payload
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or(".")
                .to_string();
            let rec = codesession::SessionRecord {
                elanus_session: session.clone(),
                native_session: native.to_string(),
                tool: "claude".to_string(),
                agent_noun: agent.clone(),
                workdir,
            };
            let _ = codesession::upsert_record(root, &rec);
        }
    }

    // Route event-mapping through the adapter the launcher recorded as the
    // session's agent noun. An unknown noun (a future adapter this binary
    // predates) still files the event generically rather than dropping it.
    let (leaf, body) = match Tool::from_agent_noun(&agent) {
        Some(tool) => tool.map_event(event, &payload),
        None => generic_event(event, &payload),
    };
    publish_obs(
        root,
        &principal,
        &token,
        &obs_topic(&agent, &session, &leaf),
        body,
    );
    Ok(())
}

/// Map a Claude Code hook event + its stdin payload to an obs/ topic leaf and a
/// trimmed body. The grammar matches src/exec.rs:
/// `tool/<name>/{call,result}` for the tool loop, plus session/turn leaves.
/// The hook stdin payload includes `session_id`, `cwd`, `permission_mode`,
/// `hook_event_name`, plus event-specific fields (Appendix A). The Codex adapter
/// adds a sibling `codex_map_event` and an arm in `Tool::map_event`.
fn claude_map_event(event: &str, payload: &Value) -> (String, Value) {
    let ts = json!(now_iso());
    let cc_session = payload.get("session_id").cloned().unwrap_or(Value::Null);
    let cwd = payload.get("cwd").cloned().unwrap_or(Value::Null);
    let common = |mut v: Value| {
        if let Value::Object(m) = &mut v {
            m.insert("ts".into(), ts.clone());
            m.insert("cc_session".into(), cc_session.clone());
        }
        v
    };
    match event {
        "SessionStart" | "Setup" => (
            "session/started".into(),
            common(json!({ "cwd": cwd, "source": payload.get("source") })),
        ),
        "UserPromptSubmit" => (
            "user/message".into(),
            common(json!({ "prompt": clip_opt(payload.get("prompt"), 4000) })),
        ),
        "PreToolUse" => {
            let tool = tool_name(payload);
            (
                format!("tool/{}/call", topic::encode_segment(&tool)),
                common(json!({ "tool": tool, "input": clip_value(payload.get("tool_input"), 4000) })),
            )
        }
        "PostToolUse" | "PostToolUseFailure" => {
            let tool = tool_name(payload);
            let failed = event == "PostToolUseFailure";
            (
                format!("tool/{}/result", topic::encode_segment(&tool)),
                common(json!({
                    "tool": tool,
                    "failed": failed,
                    "input": clip_value(payload.get("tool_input"), 2000),
                    "response": clip_value(payload.get("tool_response"), 4000),
                })),
            )
        }
        "Stop" | "StopFailure" | "SessionEnd" => (
            "session/idle".into(),
            common(json!({ "event": event, "reason": payload.get("reason") })),
        ),
        // Anything else we did not explicitly model still lands on the bus,
        // tagged by its event name, so nothing is silently dropped.
        other => {
            let (leaf, body) = generic_event(other, payload);
            (leaf, common(body))
        }
    }
}

/// Fallback mapping for an event no adapter explicitly modeled (or whose adapter
/// this binary predates): file it under `event/<name>` so nothing is silently
/// dropped. Carries no adapter-specific common fields — the caller adds those if
/// it has them.
fn generic_event(event: &str, _payload: &Value) -> (String, Value) {
    (
        format!("event/{}", topic::encode_segment(event)),
        json!({ "event": event, "ts": now_iso() }),
    )
}

/// Generate the Claude Code `--settings` object: only hooks, each routing to
/// `elanus code hook <event>`. The matcher `*` matches every tool. We record the
/// documented events for a coarse, ordered ledger (Appendix A hook event set).
fn claude_settings(self_exe: &Path, root: &Root) -> Value {
    let exe = self_exe.display().to_string();
    let root_arg = root.dir.display().to_string();
    // A single hook command shape: `<elanus> -C <root> code hook <Event>`.
    let cmd = |event: &str| {
        json!({
            "hooks": [{
                "type": "command",
                "command": format!("{exe} -C {root_arg} code hook {event}"),
            }]
        })
    };
    // Tool-loop hooks take a matcher ("*" = every tool); session/turn hooks do
    // not. This is the documented Claude Code settings.hooks schema.
    let tool_hook = |event: &str| {
        json!([{
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": format!("{exe} -C {root_arg} code hook {event}"),
            }]
        }])
    };
    json!({
        "hooks": {
            "SessionStart": [cmd("SessionStart")],
            "UserPromptSubmit": [cmd("UserPromptSubmit")],
            "PreToolUse": tool_hook("PreToolUse"),
            "PostToolUse": tool_hook("PostToolUse"),
            "Stop": [cmd("Stop")],
            "SessionEnd": [cmd("SessionEnd")],
        }
    })
}

// ── bus publish ──────────────────────────────────────────────────────────────

/// Publish one observation to the bus as the session principal. We use the same
/// `elanus bus pub` path the webhook bridge uses (real rumqttc client →
/// broker-verified sender), authenticating with the principal/token in this
/// process's environment so the broker stamps `sender = <principal>`. Best
/// effort: a publish failure (broker down) never breaks the coding session —
/// the observation plane is QoS-0-droppable telemetry (docs/bus.md).
fn publish_obs(root: &Root, principal: &str, token: &str, topic_name: &str, body: Value) {
    // buscli::publish reads ELANUS_PACKAGE/ELANUS_BUS_TOKEN from the environment.
    // In the launcher process those aren't set (only the child's were), so set
    // them for this publish; the hook process already has them. Setting them
    // unconditionally keeps both call sites correct.
    std::env::set_var("ELANUS_PACKAGE", principal);
    std::env::set_var("ELANUS_BUS_TOKEN", token);
    if let Err(e) = buscli::publish(root, topic_name, Some(&body.to_string()), 0, false, None) {
        eprintln!("[code] obs publish to {topic_name} failed (continuing): {e:#}");
    }
}

/// Session-scoped observation topic: obs/agent/<agent>/<session>/<leaf>. Mirrors
/// src/exec.rs `obs()` exactly so coding-session telemetry shares the grammar.
fn obs_topic(agent: &str, session: &str, leaf: &str) -> String {
    format!(
        "obs/agent/{}/{}/{leaf}",
        topic::encode_segment(agent),
        topic::encode_segment(session),
    )
}

// ── small helpers ────────────────────────────────────────────────────────────

fn tool_name(payload: &Value) -> String {
    payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Clip a JSON value's string form to `max` chars so a giant tool input/response
/// cannot bloat the observation. Returns Null for absent.
fn clip_value(v: Option<&Value>, max: usize) -> Value {
    match v {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => json!(clip(s, max)),
        Some(other) => json!(clip(&other.to_string(), max)),
    }
}

fn clip_opt(v: Option<&Value>, max: usize) -> Value {
    match v.and_then(Value::as_str) {
        Some(s) => json!(clip(s, max)),
        None => Value::Null,
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…[clipped {} chars]", s.chars().count() - max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn tool_parse() {
        assert!(matches!(Tool::parse("claude"), Ok(Tool::ClaudeCode)));
        assert!(matches!(Tool::parse("cc"), Ok(Tool::ClaudeCode)));
        assert!(matches!(Tool::parse("codex"), Ok(Tool::Codex)));
        assert!(Tool::parse("nonsense").is_err());
    }

    #[test]
    fn capture_strategy_and_agent_noun_per_tool() {
        // CC uses the hook bridge and generates settings; Codex uses the JSONL
        // stream and generates NO settings (no hooks, no home pollution).
        assert!(matches!(Tool::ClaudeCode.capture(), Capture::HookBridge));
        assert!(matches!(Tool::Codex.capture(), Capture::StreamJson));
        assert_eq!(Tool::Codex.agent_noun(), "codex");
        assert_eq!(Tool::Codex.binary(), "codex");
        assert!(matches!(Tool::from_agent_noun("codex"), Some(Tool::Codex)));
        let dummy_root = Root {
            dir: PathBuf::from("/tmp/fake-root"),
        };
        assert!(Tool::Codex.settings(Path::new("/usr/local/bin/elanus"), &dummy_root).is_none());
        assert!(Tool::ClaudeCode.settings(Path::new("/usr/local/bin/elanus"), &dummy_root).is_some());
    }

    #[test]
    fn obs_topic_matches_exec_grammar() {
        // Same shape as src/exec.rs obs_tool: obs/agent/<agent>/<sess>/tool/<name>/<leaf>.
        let t = obs_topic("claude-code", "code-abcd1234", "tool/Bash/call");
        assert_eq!(t, "obs/agent/claude-code/code-abcd1234/tool/Bash/call");
        assert!(topic::valid_name(&t));
        assert!(topic::matches("obs/agent/claude-code/+/tool/#", &t));
    }

    #[test]
    fn obs_topic_encodes_unsafe_segments() {
        // A wildcard in the agent/session can't escape its level.
        let t = obs_topic("a+b", "s#1", "session/start");
        assert!(topic::valid_name(&t));
        assert!(t.contains("a%2Bb"));
        assert!(t.contains("s%231"));
    }

    #[test]
    fn map_pretooluse_is_a_tool_call() {
        let payload = json!({
            "session_id": "cc-123",
            "cwd": "/tmp/proj",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": "ls -la" },
        });
        let (leaf, body) = Tool::ClaudeCode.map_event("PreToolUse", &payload);
        assert_eq!(leaf, "tool/Bash/call");
        assert_eq!(body["tool"], "Bash");
        assert_eq!(body["cc_session"], "cc-123");
        assert!(body["ts"].is_string());
        // the input is carried (clipped form), so the command is reconstructable
        assert!(body["input"].as_str().unwrap().contains("ls -la"));
    }

    #[test]
    fn map_posttooluse_marks_failure_and_carries_response() {
        let payload = json!({
            "session_id": "cc-123",
            "hook_event_name": "PostToolUseFailure",
            "tool_name": "Write",
            "tool_input": { "file_path": "/x" },
            "tool_response": "permission denied",
        });
        let (leaf, body) = Tool::ClaudeCode.map_event("PostToolUseFailure", &payload);
        assert_eq!(leaf, "tool/Write/result");
        assert_eq!(body["failed"], true);
        assert!(body["response"].as_str().unwrap().contains("permission denied"));
    }

    #[test]
    fn map_user_prompt_and_stop() {
        let (leaf, body) = Tool::ClaudeCode.map_event(
            "UserPromptSubmit",
            &json!({ "prompt": "fix the bug", "session_id": "cc" }),
        );
        assert_eq!(leaf, "user/message");
        assert_eq!(body["prompt"], "fix the bug");

        let (leaf, _) = Tool::ClaudeCode.map_event("Stop", &json!({ "session_id": "cc" }));
        assert_eq!(leaf, "session/idle");
    }

    #[test]
    fn unknown_event_still_lands() {
        let (leaf, body) = Tool::ClaudeCode.map_event("PreCompact", &json!({ "session_id": "cc" }));
        assert_eq!(leaf, "event/PreCompact");
        assert_eq!(body["event"], "PreCompact");
    }

    #[test]
    fn settings_only_contains_hooks_and_points_at_elanus() {
        let root = Root {
            dir: PathBuf::from("/tmp/fake-root"),
        };
        let s = claude_settings(Path::new("/usr/local/bin/elanus"), &root);
        // Exactly one top-level key: hooks (no user settings, no MCP, nothing
        // that would touch ~/.claude).
        let obj = s.as_object().unwrap();
        assert_eq!(obj.len(), 1);
        assert!(obj.contains_key("hooks"));
        // Every command routes through `elanus code hook`.
        let pre = &s["hooks"]["PreToolUse"][0]["hooks"][0]["command"];
        let cmd = pre.as_str().unwrap();
        assert!(cmd.contains("/usr/local/bin/elanus"));
        assert!(cmd.contains("-C /tmp/fake-root"));
        assert!(cmd.ends_with("code hook PreToolUse"));
        // Tool hooks carry a matcher; session hooks do not.
        assert_eq!(s["hooks"]["PreToolUse"][0]["matcher"], "*");
        assert!(s["hooks"]["SessionStart"][0].get("matcher").is_none());
    }

    #[test]
    fn clip_bounds_length() {
        assert_eq!(clip("short", 10), "short");
        let c = clip(&"x".repeat(100), 10);
        assert!(c.starts_with(&"x".repeat(10)));
        assert!(c.contains("clipped 90"));
    }

    // ── Codex `exec --json` stream mapping ───────────────────────────────────

    #[test]
    fn codex_thread_started_is_its_own_leaf_not_a_second_session_start() {
        // thread.started carries Codex's thread id; the launcher already emitted
        // its own session/start, so this must NOT be a second session/start.
        let (leaf, body) = codex_map_event(&json!({
            "type": "thread.started",
            "thread_id": "019ee252-3d31-7681-b1d7-7a4b3c494fb5",
        }))
        .unwrap();
        assert_eq!(leaf, "session/thread");
        assert_eq!(body["codex_thread"], "019ee252-3d31-7681-b1d7-7a4b3c494fb5");
        assert!(body["ts"].is_string());
    }

    #[test]
    fn codex_turn_started_is_skipped_completed_carries_usage() {
        // A bare turn marker is dropped.
        assert!(codex_map_event(&json!({ "type": "turn.started" })).is_none());
        // turn.completed carries the token usage (the cost signal) and lands as
        // session/idle.
        let (leaf, body) = codex_map_event(&json!({
            "type": "turn.completed",
            "usage": {
                "input_tokens": 52818,
                "cached_input_tokens": 49408,
                "output_tokens": 38,
                "reasoning_output_tokens": 0
            }
        }))
        .unwrap();
        assert_eq!(leaf, "session/idle");
        assert_eq!(body["event"], "turn.completed");
        assert_eq!(body["usage"]["input_tokens"], 52818);
        assert_eq!(body["usage"]["output_tokens"], 38);
    }

    #[test]
    fn codex_agent_message_is_an_assistant_message() {
        // Confirmed live shape: {"type":"item.completed","item":{"id":"item_1",
        // "type":"agent_message","text":"hello"}}.
        let (leaf, body) = codex_map_event(&json!({
            "type": "item.completed",
            "item": { "id": "item_1", "type": "agent_message", "text": "hello" }
        }))
        .unwrap();
        assert_eq!(leaf, "assistant/message");
        assert_eq!(body["text"], "hello");
        assert_eq!(body["item_id"], "item_1");
        // The started form of an agent_message has no settled text → dropped.
        assert!(codex_map_event(&json!({
            "type": "item.started",
            "item": { "id": "item_1", "type": "agent_message", "text": "" }
        }))
        .is_none());
    }

    #[test]
    fn codex_command_execution_maps_call_then_result() {
        // Confirmed live shapes: item.started (in_progress) is the call;
        // item.completed (exit_code+aggregated_output) is the result.
        let (call_leaf, call_body) = codex_map_event(&json!({
            "type": "item.started",
            "item": {
                "id": "item_0", "type": "command_execution",
                "command": "/bin/zsh -lc 'echo hello'",
                "aggregated_output": "", "exit_code": null, "status": "in_progress"
            }
        }))
        .unwrap();
        assert_eq!(call_leaf, "tool/command_execution/call");
        assert_eq!(call_body["tool"], "command_execution");
        assert!(call_body["command"].as_str().unwrap().contains("echo hello"));

        let (res_leaf, res_body) = codex_map_event(&json!({
            "type": "item.completed",
            "item": {
                "id": "item_0", "type": "command_execution",
                "command": "/bin/zsh -lc 'echo hello'",
                "aggregated_output": "hello\n", "exit_code": 0, "status": "completed"
            }
        }))
        .unwrap();
        assert_eq!(res_leaf, "tool/command_execution/result");
        assert_eq!(res_body["failed"], false);
        assert_eq!(res_body["exit_code"], 0);
        assert!(res_body["output"].as_str().unwrap().contains("hello"));
    }

    #[test]
    fn codex_command_nonzero_exit_is_failed() {
        let (_, body) = codex_map_event(&json!({
            "type": "item.completed",
            "item": {
                "id": "item_2", "type": "command_execution",
                "command": "false", "aggregated_output": "", "exit_code": 1,
                "status": "completed"
            }
        }))
        .unwrap();
        assert_eq!(body["failed"], true);
        assert_eq!(body["exit_code"], 1);
    }

    #[test]
    fn codex_file_change_is_a_file_write() {
        let (leaf, body) = codex_map_event(&json!({
            "type": "item.completed",
            "item": {
                "id": "item_3", "type": "file_change", "status": "completed",
                "changes": [{ "path": "src/foo.rs", "kind": "update" }]
            }
        }))
        .unwrap();
        assert_eq!(leaf, "file/write");
        assert!(body["changes"].as_str().unwrap().contains("src/foo.rs"));
        // started has no settled change → dropped.
        assert!(codex_map_event(&json!({
            "type": "item.started",
            "item": { "id": "item_3", "type": "file_change", "status": "in_progress" }
        }))
        .is_none());
    }

    #[test]
    fn codex_mcp_tool_call_maps_call_and_result_by_tool_name() {
        let (leaf, body) = codex_map_event(&json!({
            "type": "item.completed",
            "item": {
                "id": "item_4", "type": "mcp_tool_call",
                "server": "fetch", "tool_name": "get", "status": "completed",
                "arguments": { "url": "https://x" }, "result": { "ok": true }
            }
        }))
        .unwrap();
        assert_eq!(leaf, "tool/get/result");
        assert_eq!(body["tool"], "get");
        assert_eq!(body["server"], "fetch");
    }

    #[test]
    fn codex_web_search_and_todo_and_reasoning() {
        let (leaf, body) = codex_map_event(&json!({
            "type": "item.completed",
            "item": { "id": "i", "type": "web_search", "query": "rust mqtt" }
        }))
        .unwrap();
        assert_eq!(leaf, "tool/web_search/result");
        assert!(body["query"].as_str().unwrap().contains("rust mqtt"));

        let (leaf, _) = codex_map_event(&json!({
            "type": "item.completed",
            "item": { "id": "i", "type": "reasoning", "text": "thinking" }
        }))
        .unwrap();
        assert_eq!(leaf, "assistant/reasoning");

        let (leaf, _) = codex_map_event(&json!({
            "type": "item.completed",
            "item": { "id": "i", "type": "todo_list", "items": [] }
        }))
        .unwrap();
        assert_eq!(leaf, "assistant/todo");
    }

    #[test]
    fn codex_turn_failed_and_top_level_error_are_recorded() {
        let (leaf, body) = codex_map_event(&json!({
            "type": "turn.failed", "error": { "message": "boom" }
        }))
        .unwrap();
        assert_eq!(leaf, "session/idle");
        assert_eq!(body["event"], "turn.failed");
        assert!(body["error"].as_str().unwrap().contains("boom"));

        let (leaf, body) = codex_map_event(&json!({
            "type": "error", "message": "usage limit"
        }))
        .unwrap();
        assert_eq!(leaf, "session/idle");
        assert!(body["error"].as_str().unwrap().contains("usage limit"));
    }

    #[test]
    fn codex_unknown_item_type_lands_generically_nothing_dropped() {
        // An item type this binary doesn't model still lands (on completed),
        // tagged by type, so nothing is silently dropped.
        let (leaf, body) = codex_map_event(&json!({
            "type": "item.completed",
            "item": { "id": "i", "type": "some_future_item" }
        }))
        .unwrap();
        assert_eq!(leaf, "item/some_future_item");
        assert_eq!(body["item_type"], "some_future_item");

        // An unknown top-level event type also lands.
        let (leaf, body) = codex_map_event(&json!({ "type": "future.event" })).unwrap();
        assert_eq!(leaf, "event/future.event");
        assert_eq!(body["codex_event"], "future.event");
    }

    #[test]
    fn session_token_is_scoped_not_full_authority() {
        // The launch path must mint a GRANT-SCOPED session token, NOT a
        // full-authority fenced secret. Concretely: the principal must NOT
        // resolve via secrets::read (the path that yields actor=None / owner-
        // equivalent authority in the broker), and its scope must be only its
        // own obs subtree. This is the regression guard for the entry-16 gap.
        let dir = std::env::temp_dir().join(format!("elanus-codetest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = Root { dir: dir.clone() };
        let principal = "code-deadbeef";
        let token = codesession::mint(&root, principal, "claude-code", std::process::id() as i32)
            .unwrap();
        // It does NOT resolve as a full-authority fenced secret — the broker's
        // owner-equivalent path (crate::secrets::read) must return None for it.
        assert_eq!(crate::secrets::read(&root, principal), None);
        // It is scoped to exactly its own obs subtree.
        assert!(token.may_publish("obs/agent/claude-code/code-deadbeef/session/start"));
        assert!(!token.may_publish("in/human/owner"));
        assert!(!token.may_publish("work/agent/exec"));
        assert!(!token.may_subscribe("obs/#"));
        // Retire kills it.
        codesession::retire(&root, principal);
        assert!(codesession::read(&root, principal).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── The resume primitive (M2-A) ──────────────────────────────────────────

    #[test]
    fn resume_command_codex_targets_the_recorded_thread() {
        // codex resume = `codex exec resume <thread_id> --json --skip-git-repo-check
        // "<msg>"` (confirmed against codex-cli 0.141.0). The native session id from
        // the record is the resume target; the workdir is applied by the caller as
        // the child cwd (no --cd on `codex exec resume`).
        let rec = codesession::SessionRecord {
            elanus_session: "code-aaaa1111".to_string(),
            native_session: "019ee252-3d31-7681-b1d7-7a4b3c494fb5".to_string(),
            tool: "codex".to_string(),
            agent_noun: "codex".to_string(),
            workdir: "/tmp/proj".to_string(),
        };
        let (prog, args) = resume_command(&rec, "say hi again");
        assert_eq!(prog, "codex");
        assert_eq!(
            args,
            vec![
                "exec",
                "resume",
                "019ee252-3d31-7681-b1d7-7a4b3c494fb5",
                "--json",
                "--skip-git-repo-check",
                "say hi again",
            ]
        );
        // The thread id is positional right after `resume` — the resume targets THE
        // recorded thread, not --last.
        assert_eq!(args[2], rec.native_session);
    }

    #[test]
    fn resume_command_claude_resumes_the_recorded_session_headlessly() {
        // claude resume = `claude -p --resume <session_id> --output-format
        // stream-json --verbose "<msg>"` (confirmed against Claude Code 2.1.183).
        // Headless print, resuming the recorded native session id, capturing the
        // JSONL result stream (hooks are not reloaded on a bare -p --resume).
        let rec = codesession::SessionRecord {
            elanus_session: "code-bbbb2222".to_string(),
            native_session: "cc-sess-9f".to_string(),
            tool: "claude".to_string(),
            agent_noun: "claude-code".to_string(),
            workdir: "/work".to_string(),
        };
        let (prog, args) = resume_command(&rec, "continue please");
        assert_eq!(prog, "claude");
        assert!(args.contains(&"-p".to_string()));
        let resume_pos = args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(args[resume_pos + 1], "cc-sess-9f");
        assert!(args.windows(2).any(|w| w == ["--output-format", "stream-json"]));
        assert_eq!(args.last().unwrap(), "continue please");
    }

    #[test]
    fn claude_stream_result_and_message_map_to_obs_grammar() {
        // The print-stream `result` settle → session/idle carrying the answer text.
        let (leaf, body) = claude_stream_map(&json!({
            "type": "result",
            "subtype": "success",
            "session_id": "cc-sess-9f",
            "result": "done!",
            "is_error": false,
            "usage": { "input_tokens": 10, "output_tokens": 3 }
        }))
        .unwrap();
        assert_eq!(leaf, "session/idle");
        assert_eq!(body["event"], "result");
        assert_eq!(body["cc_session"], "cc-sess-9f");
        assert!(body["result"].as_str().unwrap().contains("done!"));

        // An assistant text turn → assistant/message.
        let (leaf, body) = claude_stream_map(&json!({
            "type": "assistant",
            "session_id": "cc-sess-9f",
            "message": { "content": [{ "type": "text", "text": "hi again" }] }
        }))
        .unwrap();
        assert_eq!(leaf, "assistant/message");
        assert_eq!(body["text"], "hi again");

        // A tool_use block → tool/<name>/call.
        let (leaf, body) = claude_stream_map(&json!({
            "type": "assistant",
            "session_id": "cc-sess-9f",
            "message": { "content": [{ "type": "tool_use", "name": "Bash", "input": { "command": "ls" } }] }
        }))
        .unwrap();
        assert_eq!(leaf, "tool/Bash/call");
        assert_eq!(body["tool"], "Bash");

        // The init system message → session/started (resumed session id), ONCE.
        let (leaf, body) = claude_stream_map(&json!({
            "type": "system", "subtype": "init", "session_id": "cc-sess-9f"
        }))
        .unwrap();
        assert_eq!(leaf, "session/started");
        assert_eq!(body["cc_session"], "cc-sess-9f");

        // A non-init system frame (a resume replays these) is DROPPED — so a long
        // session history does not flood the bus with duplicate starts.
        assert!(claude_stream_map(&json!({ "type": "system", "subtype": "compact" })).is_none());
        // Per-turn rate-limit telemetry is dropped (not a session happening).
        assert!(claude_stream_map(&json!({ "type": "rate_limit_event" })).is_none());
    }

    #[test]
    fn resume_errors_with_no_record() {
        // Resuming a session that was never recorded is a clean error, not a panic
        // and not a silent no-op (so a caller/test sees the missing record).
        let dir = std::env::temp_dir().join(format!("elanus-resume-norec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = Root { dir: dir.clone() };
        let err = resume(&root, "code-nope0000", "hi").unwrap_err();
        assert!(format!("{err:#}").contains("no resumable coding session"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
