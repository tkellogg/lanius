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
use std::io::Read as _;
use std::path::Path;

/// Env vars the launcher sets for the child coding-agent process tree, read back
/// by `elanus code hook` so each hook event publishes as the session principal.
const ENV_SESSION: &str = "ELANUS_CODE_SESSION";
const ENV_AGENT: &str = "ELANUS_CODE_AGENT";

/// The supported adapters. Today only Claude Code; Codex is the next increment.
#[derive(Clone, Copy)]
enum Tool {
    ClaudeCode,
}

impl Tool {
    fn parse(s: &str) -> Result<Tool> {
        match s {
            "claude" | "claude-code" | "cc" => Ok(Tool::ClaudeCode),
            "codex" => bail!(
                "the codex adapter is not built yet (next increment); only `claude` is wired"
            ),
            other => bail!("unknown coding tool {other:?} (supported: claude)"),
        }
    }
    /// The agent noun this tool's sessions publish under: obs/agent/<noun>/...
    fn agent_noun(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "claude-code",
        }
    }
    /// Recover the adapter from the agent noun the launcher recorded in the
    /// session env — so the hook bridge routes event-mapping through the right
    /// adapter without re-parsing the tool name. None for an unknown noun (a
    /// future adapter whose launcher set a noun this binary doesn't know).
    fn from_agent_noun(noun: &str) -> Option<Tool> {
        match noun {
            "claude-code" => Some(Tool::ClaudeCode),
            _ => None,
        }
    }
    /// The real binary to launch.
    fn binary(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "claude",
        }
    }
    /// The generated tool config that routes this adapter's hook events through
    /// `elanus code hook <Event>` to the bus. Dispatches to the adapter-specific
    /// generator so the Codex adapter slots in by adding an arm here.
    fn settings(self, self_exe: &Path, root: &Root) -> Value {
        match self {
            Tool::ClaudeCode => claude_settings(self_exe, root),
        }
    }
    /// Map one of this adapter's hook events + its payload to an obs/ topic leaf
    /// and a trimmed body. Adapter-specific (the hook event names and payload
    /// shapes differ per tool); the Codex adapter adds its own arm.
    fn map_event(self, event: &str, payload: &Value) -> (String, Value) {
        match self {
            Tool::ClaudeCode => claude_map_event(event, payload),
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

    // Generated hook config in the session's run scratch — never ~/.claude.
    let scratch = root.run_dir().join(&session);
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("creating run scratch {}", scratch.display()))?;
    let settings_path = scratch.join("settings.json");

    let self_exe = std::env::current_exe().context("locating the elanus binary for hook commands")?;
    let result = (|| -> Result<std::process::ExitStatus> {
        let settings = tool.settings(&self_exe, root);
        std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)
            .with_context(|| format!("writing {}", settings_path.display()))?;

        // Session start (the first ordered record): timestamp + the resolved
        // workdir, so the bus shows when and where the session began.
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

        // Launch the real binary with the generated, isolated config. The TUI
        // gets inherited stdio so it is a normal, fully usable session.
        // `--setting-sources ''` loads NO user/project/local settings (the
        // user's ~/.claude hooks/CLAUDE.md are untouched); `--settings <file>`
        // loads only our generated hooks (Appendix A).
        let mut cmd = std::process::Command::new(tool.binary());
        cmd.arg("--settings")
            .arg(&settings_path)
            .arg("--setting-sources")
            .arg("");
        cmd.args(args);
        // The session's own identity, carried to the hook bridge children CC
        // spawns. ELANUS_PACKAGE + ELANUS_BUS_TOKEN are what `elanus bus pub`
        // authenticates with (src/buscli.rs); ELANUS_CODE_* tell the bridge
        // which session/agent to file events under.
        cmd.env("ELANUS_PACKAGE", &principal)
            .env("ELANUS_BUS_TOKEN", &bus_token)
            .env(ENV_SESSION, &session)
            .env(ENV_AGENT, &agent)
            .env("ELANUS_ROOT", &root.dir);
        eprintln!("[code] launching {} as session {session}", tool.binary());
        cmd.status()
            .with_context(|| format!("launching {} (is it installed and on PATH?)", tool.binary()))
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
        assert!(Tool::parse("codex").is_err());
        assert!(Tool::parse("nonsense").is_err());
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
}
