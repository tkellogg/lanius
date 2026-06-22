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
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};

/// Env vars the launcher sets for the child coding-agent process tree, read back
/// by `elanus code hook` so each hook event publishes as the session principal.
const ENV_SESSION: &str = "ELANUS_CODE_SESSION";
const ENV_AGENT: &str = "ELANUS_CODE_AGENT";

/// Internal launch-control env vars used by `elanus code spawn`.
///
/// `ELANUS_CODE_FORCE_SESSION` lets a detached wrapper process use the worker
/// handle its spawner already printed, after validating that it is a safe
/// `code-*` principal. `ELANUS_CODE_REPLY_TO` names the spawning session that
/// should receive the worker's completion, and
/// `ELANUS_CODE_REPLY_CORRELATION` threads that completion through the same
/// conversation id the spawn command reported. These vars are consumed by the
/// elanus wrapper only; they must NOT leak onward into the real coding tool, or a
/// nested `elanus code <tool>` launched by the worker could accidentally inherit
/// the forced id / reply route.
const ENV_FORCE_SESSION: &str = "ELANUS_CODE_FORCE_SESSION";
const ENV_REPLY_TO: &str = "ELANUS_CODE_REPLY_TO";
const ENV_REPLY_CORRELATION: &str = "ELANUS_CODE_REPLY_CORRELATION";
/// Spawn-depth guard carried through detached workers. Unlike the reply/force
/// launch-control vars, this MUST propagate into the real tool child so nested
/// `elanus code spawn` calls see and increment the current depth.
const ENV_SPAWN_DEPTH: &str = "ELANUS_CODE_SPAWN_DEPTH";
/// Hard cap on recursively spawned detached workers. It is intentionally roomy
/// for normal delegation trees but stops accidental fork-bomb prompts.
const MAX_SPAWN_DEPTH: u32 = 8;

/// One-turn teaching nudge surfaced only when the user's submitted prompt is
/// plausibly asking for delegation, parallelism, or another coding agent.
const DISPATCH_HINT: &str = "[elanus] Tip: you can dispatch coding workers yourself - run `elanus code help` for all verbs. Live/blocking: `elanus code codex \"<task>\"` runs a Codex worker and returns its result inline; `elanus code opencode \"<task>\"` runs an opencode worker; `elanus code claude --worker \"<task>\"` runs a headless Claude worker.";

/// The session-local Claude Code skill body written under the run scratch. Claude
/// discovers it through `--add-dir <scratch>/skillroot` loading
/// `.claude/skills/elanus`, so `/elanus` is available only for this session and
/// vanishes with the scratch without exposing generated settings.json.
const ELANUS_SKILL: &str = r#"---
name: elanus
description: Shows how to dispatch coding workers from this elanus-launched Claude Code session.
---

# elanus worker dispatch

Use this cheatsheet when you need another coding worker:

- Full help: `elanus code help`
- Live/blocking Codex worker: `elanus code codex "<task>"`
- Live/blocking opencode worker: `elanus code opencode "<task>"`
- Live/blocking Claude worker: `elanus code claude --worker "<task>"`
- Async spawn: `elanus code spawn <tool> "<task>"`
- Async deliver to an existing worker: `elanus code deliver <worker> "<msg>"`
- Check your own mailbox: `elanus code inbox`

For async `spawn` or `deliver`, end your turn after dispatch. Do not poll, sleep,
or wait; elanus wakes you later with the result.
"#;

/// Provider-credential env vars scrubbed from EVERY launched/resumed coding-agent
/// child (Task 2 / docs/handoffs/coding-agents.md cred-scrub Log).
///
/// elanus loads its own `.env` into its process (`dotenv::load` in main.rs) so its
/// NATIVE agents can reach a provider through the genai anthropic-compat path — in
/// the field that `.env` points `ANTHROPIC_BASE_URL`/`ANTHROPIC_API_KEY` at a
/// DeepSeek endpoint. A spawned coding tool (Claude Code / Codex) would otherwise
/// INHERIT those vars and be misdirected away from its own login (`~/.claude` /
/// `~/.codex`). The coding tool brings its OWN auth, so the launcher must NOT leak
/// elanus's provider credentials into it: each spawn `env_remove`s these before
/// exec. This scrubs ONLY provider credentials — the `ELANUS_*` session/bus/root
/// vars the hook bridge and `elanus code …` children depend on are set explicitly
/// AFTER the scrub and are never in this list.
///
/// (A future `--inherit-credentials` flag could opt back in to passing them, for a
/// user who deliberately wants the tool to use elanus's provider; not built — the
/// correct default is the tool's own auth.)
const PROVIDER_CRED_VARS: &[&str] = &[
    // Anthropic / Claude Code (and the genai anthropic-compat path elanus uses).
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
    // OpenAI / Codex (and any OpenAI-compatible provider elanus is pointed at).
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENAI_API_BASE",
    "OPENAI_MODEL",
];

/// Scrub elanus's provider credentials from a child `Command` before it spawns the
/// coding tool, so the tool uses its OWN login rather than inheriting elanus's
/// provider env (see `PROVIDER_CRED_VARS`). Returns the same `&mut Command` for
/// chaining. The `ELANUS_*` session/bus/root vars the bridge needs are set by the
/// caller AFTER this and are deliberately NOT scrubbed.
fn scrub_provider_creds(cmd: &mut std::process::Command) -> &mut std::process::Command {
    for var in PROVIDER_CRED_VARS {
        cmd.env_remove(var);
    }
    cmd
}

/// Remove internal launch-control variables from a real coding-tool child. They
/// are instructions to this elanus wrapper, not part of the session identity the
/// model should inherit. The wrapper sets fresh `ELANUS_CODE_SESSION` /
/// `ELANUS_CODE_AGENT` for the tool after this scrub.
fn scrub_launch_control_env(cmd: &mut std::process::Command) -> &mut std::process::Command {
    for var in [ENV_FORCE_SESSION, ENV_REPLY_TO, ENV_REPLY_CORRELATION] {
        cmd.env_remove(var);
    }
    cmd
}

pub fn print_help() {
    println!(
        "\
Usage: elanus code <verb> [args...]

Launch tools:
  elanus code claude [args...]          launch Claude Code
  elanus code claude --worker \"<task>\"   run Claude headless and print its result
  elanus code codex \"<task>\"           launch Codex; the task is positional
  elanus code opencode \"<task>\"        launch opencode; the task is positional

Commands:
  elanus code deliver <worker-session> \"<message>\"  dispatch work to a worker session
  elanus code spawn <tool> \"<task>\"                  start a worker in the background
  elanus code inbox [--all] [--json]                  show this session's inbox
  elanus code resume <elanus-session> \"<message>\"    resume a recorded session
  elanus code note <session> \"<text>\"                set or clear a session note
  elanus code claim <path>                            announce an advisory edit claim
  elanus code unclaim <path>                          release an advisory edit claim
  elanus code claims [--json]                         show edit claims in this room
  elanus code project                                  refresh the trace->sqlite session projection
  elanus code sessions [--json]                        list coding sessions + stats
  elanus code session <id> [--json]                   one session: stats, timeline, resume command
  elanus code help                                    show this help
  elanus code list                                    list supported launch tools
  elanus code hook <event>                            internal hook bridge"
    );
}

pub fn print_tools() {
    println!("claude");
    println!("codex");
    println!("opencode");
}

/// The supported adapters: Claude Code (hook bridge), Codex (`exec --json` stdout
/// stream), and opencode (`run --format json` stdout stream). They share the
/// envelope; only the capture mechanism differs.
#[derive(Clone, Copy)]
enum Tool {
    ClaudeCode,
    Codex,
    OpenCode,
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
            "opencode" | "oc" => Ok(Tool::OpenCode),
            other => bail!("unknown coding tool {other:?} (supported: claude, codex, opencode)"),
        }
    }
    /// The agent noun this tool's sessions publish under: obs/agent/<noun>/...
    fn agent_noun(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "claude-code",
            Tool::Codex => "codex",
            Tool::OpenCode => "opencode",
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
            "opencode" => Some(Tool::OpenCode),
            _ => None,
        }
    }
    /// The real binary to launch.
    fn binary(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "claude",
            Tool::Codex => "codex",
            Tool::OpenCode => "opencode",
        }
    }
    /// How the launcher captures this adapter's activity (the capture seam).
    fn capture(self) -> Capture {
        match self {
            Tool::ClaudeCode => Capture::HookBridge,
            // Codex 0.141 hooks are a plugin/managed-config dead end; capture the
            // `codex exec --json` stdout stream in-process instead (Appendix B).
            Tool::Codex => Capture::StreamJson,
            // opencode `run --format json` prints a JSONL event stream on stdout
            // (no hooks, no home pollution) — same StreamJson strategy as Codex.
            Tool::OpenCode => Capture::StreamJson,
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
            // opencode is a StreamJson adapter: no hooks, no generated settings.
            Tool::OpenCode => None,
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
            // opencode is also a StreamJson adapter (no hooks); the hook bridge is
            // never reached for it. File generically if it somehow is.
            Tool::OpenCode => generic_event(event, payload),
        }
    }
}

// ── Inbound delivery: mailbox → resume (M2-B) ────────────────────────────────
//
// A coding session's mailbox is `in/agent/<tool>/<conv>` — symmetric with its
// telemetry `obs/agent/<tool>/<session>/...` (docs/topics.md: `in/` first locator
// is the conversation; here the conversation IS the session, the stable handle a
// resume targets). `<tool>` is the agent NOUN (`codex` / `claude-code`), so the
// mailbox and the obs subtree share the same first locators. The daemon (the
// kernel — it has the authority the emit-only session lacks) recognizes such an
// event, reads the durable record, and drives `resume_capture`. The session never
// gains read authority; only the daemon reads the mailbox.

/// Decode a session id the launcher encoded into a topic segment with
/// `topic::encode_segment` (percent-encodes `% + # /`). Inverse of that encoder,
/// so a recovered `code-<id>` matches the durable record's key exactly even for a
/// name carrying reserved characters. Lenient on a trailing/partial `%` (returns
/// the literal) — a malformed segment simply won't match any real session id.
fn decode_segment(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// If `topic` is a coding-session mailbox addressed to an EXISTING recorded
/// session, return its `(elanus_session, agent_noun)`. The topic must be exactly
/// `in/agent/<tool>/<conv>` where `<conv>` decodes to a `code-*` id with a durable
/// `code_sessions` record AND `<tool>` is the agent noun that record publishes
/// under (so `in/agent/codex/code-x` drives `code-x` only if it is a codex
/// session — a mismatched noun is ignored, not cross-driven). Returns None for
/// anything else: a non-mailbox topic, an unknown/never-recorded conv, a
/// non-`code-*` conv (an ordinary agent's mailbox), or a noun/record mismatch —
/// so a delivery to a non-session address is cleanly ignored (no panic, no
/// spurious resume). The daemon calls this on every materialized `in/` event.
pub fn recognize_delivery(root: &Root, topic_name: &str) -> Option<(String, String)> {
    let segs: Vec<&str> = topic_name.split('/').collect();
    // Exactly four levels: in / agent / <tool> / <conv>. A finer-grained
    // sub-conversation locator (`in/agent/<tool>/<conv>/<thread>`) is NOT a
    // session drive in M2-B — keep recognition tight so only the documented
    // address resumes.
    if segs.len() != 4 || segs[0] != "in" || segs[1] != "agent" {
        return None;
    }
    let conv = decode_segment(segs[3]);
    // Cheap structural gate before any db read: only `code-*` convs can be a
    // coding session, and the name must be a valid session principal.
    if !codesession::is_session_principal(&conv) {
        return None;
    }
    let rec = codesession::read_record(root, &conv).ok().flatten()?;
    // The mailbox noun must be the noun this session publishes under, so a
    // delivery to `in/agent/codex/<conv>` drives a codex session only, never a
    // claude-code one with the same id (ids are globally unique, but this keeps
    // the address honest and rejects a typo'd noun rather than cross-driving).
    if decode_segment(segs[2]) != rec.agent_noun {
        return None;
    }
    Some((rec.elanus_session, rec.agent_noun))
}

/// Who to route a worker's completion back to (M4-A). When a planner hands work
/// to a worker, the completion must reach the planner so it resumes to react —
/// closing the orchestration loop. The requester is captured from the inbound
/// delivery and stored with the in-flight job so `settle_code_deliveries` can
/// publish the completion to the requester's mailbox carrying the same
/// `correlation_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRequester {
    /// The mailbox topic to deliver the completion to, e.g.
    /// `in/agent/claude-code/code-<planner>`. If the requester is itself a coding
    /// session this is its session mailbox, so the existing M2-B machinery
    /// resumes it — that is the loop closing.
    pub reply_to: String,
}

/// Determine where a worker's completion should be routed for an inbound
/// delivery, from the payload's explicit `reply_to` and the broker-verified
/// `sender` of the delivery event. Precedence:
///
/// 1. An explicit `reply_to` in the payload — it must name a **recognized actor**,
///    not an arbitrary topic. It is ALWAYS resolved through `mailbox_for_actor`
///    (the same safe path the sender-derived route uses): a coding session
///    (`code-*` with a durable record) → its own session mailbox; a valid agent
///    name → `in/agent/<agent>/<conv>`. Two input forms are accepted, both
///    resolved (never used verbatim): a bare actor NAME (`code-<id>` or an agent
///    noun), or a full `in/agent/<noun>/<conv>` mailbox topic (from which the
///    actor is extracted and re-derived). Anything else — a raw/arbitrary `in/...`
///    topic, `in/human/*`, `signal/`, `obs/`, a wildcard, a path-unsafe name — is
///    REJECTED (returns None), so a kernel-authored completion can never be
///    published to the human inbox or an arbitrary topic via `reply_to`
///    (docs/security.md: confused-deputy).
/// 2. Otherwise the `sender` the broker stamped on the delivery — the genuine,
///    unforgeable requester. A coding-session sender (`code-*`) is expanded to its
///    own mailbox (`in/agent/<its-noun>/<sender>`) so the completion resumes it; a
///    native agent sender becomes `in/agent/<sender>/<conv>`.
///
/// Returns None when there is no requester to route to (the `kernel`/owner
/// senders that originate a delivery with no one waiting on a coding completion,
/// or an unresolvable reply_to) — a normal worker resume with no routing, so an
/// ordinary delivery with no planner still works unchanged.
pub fn delivery_requester(
    root: &Root,
    payload: &Value,
    sender: Option<&str>,
    correlation: Option<&str>,
) -> Option<DeliveryRequester> {
    // 1. An explicit reply_to in the payload wins — but it must RESOLVE to a known
    //    actor's mailbox, never be routed verbatim. A planner names *who* to reply
    //    to (itself, a worker, an agent), and the daemon derives the mailbox; it
    //    cannot dictate a raw topic for a kernel-authored message.
    if let Some(rt) = payload.get("reply_to").and_then(Value::as_str) {
        let rt = rt.trim();
        if !rt.is_empty() {
            return resolve_reply_to(root, rt, correlation)
                .map(|reply_to| DeliveryRequester { reply_to });
        }
    }
    // 2. Fall back to the broker-verified sender of the delivery.
    let sender = sender?.trim();
    // The kernel and the human owner are not coding planners waiting on a
    // completion — don't route a reply back to them (their delivery is a plain
    // worker resume, the existing behavior). A coding session or a native agent
    // sender IS a requester.
    if sender.is_empty() || sender == "kernel" || sender == "owner" {
        return None;
    }
    mailbox_for_actor(root, sender, correlation).map(|reply_to| DeliveryRequester { reply_to })
}

/// Resolve an explicit `reply_to` to a recognized actor's mailbox, or None if it
/// does not name one. This is the constraint that closes the confused-deputy hole:
/// the daemon routes a kernel-authored completion ONLY to an actor mailbox it
/// re-derives, never to a verbatim topic a payload chose.
///
/// Accepted forms (both resolved through `mailbox_for_actor`, the same safe path
/// the sender route uses, never used verbatim):
/// - a **bare actor name** (`code-<id>` or an agent noun): no `/`.
/// - a full **`in/agent/<noun>/<conv>`** mailbox topic: exactly four levels, the
///   actor is the (decoded) `<conv>` for a coding session, else the `<noun>`.
///
/// Rejected (None): a raw/arbitrary `in/...` topic, `in/human/*`, `in/group/*`,
/// `signal/`, `obs/`, `work/`, any wildcard, a path-unsafe name — anything that is
/// not a recognized actor address. The result is itself revalidated as a concrete
/// mailbox name before use.
fn resolve_reply_to(root: &Root, rt: &str, correlation: Option<&str>) -> Option<String> {
    // A bare actor name (no '/'): expand it to its mailbox.
    if !rt.contains('/') {
        return mailbox_for_actor(root, rt, correlation);
    }
    // A topic form is only accepted if it is a concrete in/agent/<noun>/<conv>
    // mailbox — never in/human/*, signal/*, obs/*, a room, or a wildcard. Extract
    // the actor and re-derive the mailbox through the safe path; the original
    // string is NEVER routed verbatim.
    if !topic::valid_name(rt) {
        return None; // wildcards / malformed: not routable
    }
    let segs: Vec<&str> = rt.split('/').collect();
    if segs.len() != 4 || segs[0] != "in" || segs[1] != "agent" {
        return None; // not an agent-mailbox shape (in/human/*, in/group/*, …)
    }
    let noun = decode_segment(segs[2]);
    let conv = decode_segment(segs[3]);
    // If the conversation names a coding session, route to ITS own mailbox
    // (re-derived from the durable record) — exactly the safe sender path. A
    // session with no record is not a recognized actor → None.
    if codesession::is_session_principal(&conv) {
        return mailbox_for_actor(root, &conv, correlation);
    }
    // Otherwise treat the noun as an agent name and re-derive its mailbox with the
    // named conversation (not the message's correlation — the payload addressed a
    // specific conv). A path-unsafe noun/conv is rejected by mailbox_for_actor.
    mailbox_for_actor(root, &noun, Some(&conv))
}

/// Build the mailbox topic for a bare actor name. A coding session (`code-*` with
/// a record) routes to its OWN session mailbox `in/agent/<its-noun>/<session>` so
/// the completion resumes it via M2-B. A native agent name routes to
/// `in/agent/<name>/<conv>` (the correlation as the conversation locator, falling
/// back to the agent's default conversation). None for an unusable or path-unsafe
/// name (a name with `/` or a reserved prefix could otherwise be coaxed toward a
/// non-agent topic level — reject it).
fn mailbox_for_actor(root: &Root, name: &str, correlation: Option<&str>) -> Option<String> {
    // The actor name becomes a single topic LEVEL. `encode_segment` already
    // neutralizes wildcards/`/`, but require a valid principal so a path-unsafe or
    // reserved name (`.`, traversal, an `in/`-shaped string) is rejected outright
    // rather than encoded into a junk-but-live mailbox.
    if !crate::secrets::valid_principal(name) {
        return None;
    }
    if codesession::is_session_principal(name) {
        // A coding session: deliver to its own mailbox so M2-B resumes it. Its
        // noun comes from the durable record; without one we can't address it
        // (and an unrecorded code-* name is not a recognized actor).
        let rec = codesession::read_record(root, name).ok().flatten()?;
        return Some(format!(
            "in/agent/{}/{}",
            topic::encode_segment(&rec.agent_noun),
            topic::encode_segment(name),
        ));
    }
    // A native agent (or any non-session actor): its mailbox under its name. Use
    // the correlation as the conversation locator so the planner threads it; fall
    // back to a stable default conversation when there is none.
    let conv = correlation.filter(|c| !c.is_empty()).unwrap_or("main");
    Some(format!(
        "in/agent/{}/{}",
        topic::encode_segment(name),
        topic::encode_segment(conv),
    ))
}

/// The idempotency key for an inbound delivery (M4-A). An explicit
/// `idempotency_key` in the payload wins (a planner/tool that wants exactly-once
/// across re-publishes sets it); otherwise the inbound event id, which is stable
/// across the at-least-once replay (a daemon crash re-pends the SAME row, same
/// id). Pure — no db, unit-testable.
pub fn idempotency_key(payload: &Value, event_id: i64) -> String {
    payload
        .get("idempotency_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| format!("event:{event_id}"))
}

/// Pull the message text out of a delivery payload. Accept `prompt` (the
/// documented field) or `text` (a convenience alias), in that order; a bare JSON
/// string is taken verbatim. None if neither is present (an empty/structureless
/// payload is not a drivable message — the daemon skips it rather than resume on
/// nothing).
pub fn delivery_message(payload: &Value) -> Option<String> {
    if let Some(s) = payload.as_str() {
        return Some(s.to_string());
    }
    for key in ["prompt", "text"] {
        if let Some(s) = payload.get(key).and_then(Value::as_str) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

// ── The deliver tool: a planner dispatches work to a worker (M4-B) ────────────
//
// `elanus code deliver <worker-session> "<message>"` is how a *planner* coding
// session hands work to a *worker* coding session without busy-waiting. It is the
// origination half the M4-A loop left open: M4-A routes a worker's completion back
// to whoever asked; this is how a coding-session planner *becomes* that asker.
//
// **Plumbing + record, NOT a new bus authority.** Planner and worker are both the
// user's own agents with the SAME authority — there is no trust boundary between
// them and nothing to gate. The tool does NOT use the session's bus token to
// publish into the worker's mailbox (that token is emit-only — its own obs subtree,
// nothing else — and stays that way). Instead it writes a `pending` delivery event
// straight to the kernel ledger via `events::emit`, stamped `sender = <the running
// planner session>`, exactly as the daemon's own `route_completion` does. The
// daemon's `drive_code_deliveries` picks it up next tick, drives the worker, and —
// because the recorded `sender` is the planner (a `code-*` session with a record) —
// M4-A's `delivery_requester` routes the completion back to the planner's mailbox,
// resuming it. The safety here is the audit trail: who dispatched what to whom, on
// the bus, with honest provenance — not a permission check.

/// `elanus code deliver <worker-session> "<message>"` — dispatch work to a worker.
///
/// Run from inside a coding session (the launcher sets `ELANUS_CODE_SESSION` /
/// `ELANUS_CODE_AGENT` in the child's env): that running session is recorded as the
/// **requester**, so M4-A routes the worker's completion back to it. Fails cleanly
/// if there is no running-session identity in the env, if the worker session has no
/// durable record (never launched / wrong id), or if a session tries to deliver to
/// itself (which would self-resume into a loop). The delivery carries the message,
/// the requester as `reply_to`, and a correlation, and is emitted through the
/// kernel ledger so it is recorded with provenance — the planner's emit-only token
/// is never used or widened.
pub fn deliver(root: &Root, worker_session: &str, message: &str) -> Result<()> {
    // The running session is the requester — captured from the env the launcher
    // set on this coding agent's process tree. Without it we are not inside a
    // coding session and have no honest requester to record (fail rather than
    // dispatch anonymously, which would route the completion nowhere).
    let requester = std::env::var(ENV_SESSION).ok().filter(|s| !s.is_empty());
    let Some(requester) = requester else {
        bail!(
            "elanus code deliver must run inside a coding session \
             (no {ENV_SESSION} in the environment — are you running it from a \
             session launched by `elanus code`?)"
        );
    };
    let id = record_delivery(root, &requester, worker_session, message)?;
    eprintln!("[code] delivered to {worker_session} (event {id}, from {requester})");
    println!(
        "delivered to {worker_session}: the daemon will resume it with your message; \
         its completion will be delivered back to your mailbox. End your turn now — \
         do not wait."
    );
    Ok(())
}

/// Build and record the delivery to a worker, with `requester` as the recorded
/// sender. The env-free core of `deliver` (the requester comes from the env in the
/// CLI; here it is explicit so the path is unit-testable). Returns the emitted
/// event id. Fails cleanly on an empty message, an unknown worker, or a
/// self-delivery.
pub fn record_delivery(
    root: &Root,
    requester: &str,
    worker_session: &str,
    message: &str,
) -> Result<i64> {
    let worker_session = worker_session.trim();
    if worker_session.is_empty() {
        bail!("usage: elanus code deliver <worker-session> \"<message>\"");
    }
    let message = message.trim();
    if message.is_empty() {
        bail!("a deliver message must not be empty");
    }
    let requester = requester.to_string();

    // The worker must be a real, recorded session — otherwise the delivery would
    // sit in a mailbox the daemon never resumes. Resolve its record to get the
    // agent noun for the mailbox address, and to confirm it exists.
    let rec = codesession::read_record(root, worker_session)
        .context("reading the worker session record")?
        .with_context(|| {
            format!(
                "no coding session {worker_session:?} to deliver to \
                 (never launched, or its native session id was never observed)"
            )
        })?;

    if worker_session == requester {
        bail!(
            "refusing to deliver to your own session {requester:?} \
             (a session cannot dispatch work to itself)"
        );
    }

    // The worker's mailbox: in/agent/<worker-noun>/<worker-session> — exactly the
    // address `recognize_delivery` resumes (M2-B). Encode the segments so a name
    // with reserved characters can't escape its level.
    let mailbox = format!(
        "in/agent/{}/{}",
        topic::encode_segment(&rec.agent_noun),
        topic::encode_segment(worker_session),
    );

    // An explicit reply_to: the planner's OWN session mailbox, so M4-A routes the
    // worker's completion straight back to it. The recorded `sender` alone already
    // drives M4-A's requester capture (a `code-*` sender → its own mailbox), but
    // setting reply_to makes the intent explicit and is the bare requester NAME,
    // which `delivery_requester` re-derives through `mailbox_for_actor` (never used
    // verbatim — it can't be coaxed into an arbitrary topic). We only set it when
    // the requester has a durable record (so the route is addressable); a
    // freshly-launched planner whose native id isn't observed yet omits it and
    // relies on the recorded sender once its record exists.
    let mut payload = json!({ "prompt": message });
    if codesession::read_record(root, &requester)
        .ok()
        .flatten()
        .is_some()
    {
        payload["reply_to"] = json!(requester);
    }

    // A correlation threads the whole round trip (deliver → worker → completion →
    // planner resume) as one conversation.
    let correlation = format!("code-deliver-{}", uuid::Uuid::new_v4().simple());

    // Emit through the kernel ledger as the planner session. This is the SAME path
    // the daemon's route_completion uses (events::emit with an explicit sender) —
    // it does NOT touch the session's bus token, so the emit-only scope is never
    // widened. The event is `pending`; drive_code_deliveries picks it up next tick.
    let conn = crate::db::open(root).context("opening the ledger to record the delivery")?;
    crate::db::init_schema(&conn)?;
    let id = crate::events::emit(
        root,
        &conn,
        crate::events::EmitOpts {
            payload: Some(payload),
            correlation: Some(correlation.clone()),
            sender: Some(requester.clone()),
            ..crate::events::EmitOpts::new(&mailbox)
        },
    )
    .context("recording the delivery on the ledger")?;
    Ok(id)
}

// ── The spawn tool: create a worker and route completion back (D3) ───────────
//
// `elanus code spawn <tool> "<task>"` is the async counterpart to the blocking
// foreground launch. It is deliberately just plumbing + record: the spawner and
// worker are both the user's coding sessions, so there is no new bus authority
// and no widened session token. The spawner names a tool and prompt; this command
// starts a detached elanus wrapper with a pre-generated worker id and reply route,
// then exits immediately. The wrapper mints its OWN scoped worker identity in
// `launch()`, runs the tool, and on completion records a kernel-ledger delivery
// to the spawner's mailbox via `mailbox_for_actor` (never a raw topic).

/// Generate a fresh elanus coding-session id in the existing `code-<8hex>` shape.
/// Kept as a helper so `spawn` and `launch` cannot drift apart.
fn new_code_session_id() -> String {
    format!("code-{}", &uuid::Uuid::new_v4().to_string()[..8])
}

/// Select the session id for a launch. A detached `spawn` pre-generates the
/// worker handle and passes it as `ELANUS_CODE_FORCE_SESSION`; this function uses
/// it only after `codesession::is_session_principal` accepts the name AND no
/// credential file already exists for that principal. Reusing an existing forced
/// id could clobber a live worker's token; because the liveness probe is private
/// to `codesession`, existence is treated as unsafe here and the normal random id
/// path is used. An invalid forced value is ignored with a warning, so a malformed
/// environment cannot smuggle a path-unsafe principal into the token store.
fn launch_session_id(root: &Root) -> String {
    match std::env::var(ENV_FORCE_SESSION)
        .ok()
        .filter(|s| !s.is_empty())
    {
        Some(forced) if codesession::is_session_principal(&forced) => {
            if forced_session_token_exists(root, &forced) {
                eprintln!(
                    "[code] ignoring {ENV_FORCE_SESSION}={forced:?}: \
                     session credential already exists"
                );
                new_code_session_id()
            } else {
                forced
            }
        }
        Some(forced) => {
            eprintln!("[code] ignoring invalid {ENV_FORCE_SESSION}={forced:?}");
            new_code_session_id()
        }
        None => new_code_session_id(),
    }
}

/// Does the fenced token-store path for a syntactically valid forced session id
/// already exist? `codesession::read` intentionally hides unparseable tokens; for
/// the clobber guard, even an unreadable existing file means "do not overwrite".
fn forced_session_token_exists(root: &Root, principal: &str) -> bool {
    root.secrets()
        .join("code-sessions")
        .join(format!("{principal}.json"))
        .exists()
}

/// Remove the spawner's live identity from the detached elanus wrapper process
/// before setting the worker's launch-control env. The wrapper must mint a fresh
/// worker token in `launch()`: inheriting the spawner's `ELANUS_PACKAGE`,
/// `ELANUS_BUS_TOKEN`, `ELANUS_CODE_SESSION`, or `ELANUS_CODE_AGENT` would make
/// provenance ambiguous and could route hooks as the wrong session.
fn scrub_spawn_wrapper_identity_env(cmd: &mut std::process::Command) -> &mut std::process::Command {
    for var in [
        "ELANUS_PACKAGE",
        "ELANUS_BUS_TOKEN",
        ENV_SESSION,
        ENV_AGENT,
        ENV_FORCE_SESSION,
        ENV_REPLY_TO,
        ENV_REPLY_CORRELATION,
    ] {
        cmd.env_remove(var);
    }
    cmd
}

/// `elanus code spawn <tool> "<task>"` — start a worker in the background.
///
/// Must run from inside a coding session, identified by `ELANUS_CODE_SESSION`.
/// The worker's session id is generated before the child starts and passed to the
/// detached wrapper as `ELANUS_CODE_FORCE_SESSION`; the wrapper validates that
/// forced id before minting its scoped token. `ELANUS_CODE_REPLY_TO` and
/// `ELANUS_CODE_REPLY_CORRELATION` tell the wrapper where to deliver the worker's
/// completion when `launch()` finishes. The detached wrapper inherits
/// `ELANUS_ROOT` so it resolves the same ledger/root, but it does NOT inherit the
/// spawner's session/bus identity.
pub fn spawn(root: &Root, tool: &str, prompt: &str) -> Result<()> {
    let spawner = std::env::var(ENV_SESSION).ok().filter(|s| !s.is_empty());
    let Some(spawner) = spawner else {
        bail!(
            "elanus code spawn must run inside a coding session \
             (no {ENV_SESSION} in the environment — are you running it from a \
             session launched by `elanus code`?)"
        );
    };

    let prompt = prompt.trim();
    if prompt.is_empty() {
        bail!("usage: elanus code spawn <tool> \"<task>\"");
    }

    let parsed = Tool::parse(tool)?;
    let spawn_depth = std::env::var(ENV_SPAWN_DEPTH)
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    if spawn_depth >= MAX_SPAWN_DEPTH {
        bail!("refusing to spawn: max spawn depth {MAX_SPAWN_DEPTH} reached");
    }
    let worker_session = new_code_session_id();
    let correlation = format!("code-spawn-{}", uuid::Uuid::new_v4().simple());
    let self_exe =
        std::env::current_exe().context("locating the elanus binary for background spawn")?;

    let mut cmd = std::process::Command::new(self_exe);
    cmd.arg("code").arg(parsed.binary());
    if matches!(parsed, Tool::ClaudeCode) {
        // Claude's interactive TUI cannot run with detached stdio; force the
        // headless worker shape (`claude -p`) in the background wrapper.
        cmd.arg("--worker");
    }
    cmd.arg(prompt)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    scrub_spawn_wrapper_identity_env(&mut cmd);
    cmd.env(ENV_FORCE_SESSION, &worker_session)
        .env(ENV_REPLY_TO, &spawner)
        .env(ENV_REPLY_CORRELATION, &correlation)
        .env(ENV_SPAWN_DEPTH, (spawn_depth + 1).to_string())
        .env("ELANUS_ROOT", &root.dir);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Put the worker wrapper in its own process group so it does not share
        // terminal-generated signals with this short-lived CLI process.
        cmd.process_group(0);
    }

    cmd.spawn()
        .with_context(|| format!("starting detached {tool} worker {worker_session}"))?;

    println!(
        "spawned {tool} worker {worker_session}; its result will be delivered to \
         your mailbox (correlation {correlation}). End your turn now — do not wait."
    );
    Ok(())
}

/// Build the prompt delivered back to a spawner when a spawned worker finishes.
/// The result is deliberately a single ordinary prompt string because the daemon's
/// existing mailbox→resume machinery already knows how to resume a coding session
/// from a `{"prompt": ...}` payload. It names the worker, includes exit status or
/// launch error, carries the worker's clipped final text, and lists the files the
/// capture path observed changing.
fn completion_delivery_prompt(
    worker_session: &str,
    status: Option<&std::process::ExitStatus>,
    summary: &CaptureSummary,
    launch_error: Option<&str>,
) -> String {
    let status_line = if let Some(err) = launch_error {
        format!("launch error: {}", clip(err, 2000))
    } else if let Some(status) = status {
        match status.code() {
            Some(124) => format!("timed out after {}s", spawn_timeout_secs()),
            Some(code) => format!("exit code {code}"),
            None if status.success() => "success".to_string(),
            None => "terminated without an exit code".to_string(),
        }
    } else {
        "status unavailable".to_string()
    };
    let final_text = summary
        .final_text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| clip(s, FINAL_TEXT_CAP))
        .unwrap_or_else(|| "(no final text)".to_string());
    let files = if summary.file_changes.is_empty() {
        "(none)".to_string()
    } else {
        clipped_file_changes(&summary.file_changes)
    };
    format!(
        "Worker {worker_session} finished.\n\
         Status: {status_line}\n\n\
         Final text:\n{final_text}\n\n\
         Files changed: {files}"
    )
}

/// Render a bounded file-change list for routed completion prompts. Worker tools
/// can report very large change sets; the delivery should wake the spawner with
/// useful context without turning the mailbox item into an unbounded path dump.
fn clipped_file_changes(paths: &[String]) -> String {
    const MAX_COMPLETION_FILE_CHANGES: usize = 50;
    let mut out = paths
        .iter()
        .take(MAX_COMPLETION_FILE_CHANGES)
        .cloned()
        .collect::<Vec<_>>();
    if paths.len() > MAX_COMPLETION_FILE_CHANGES {
        out.push(format!(
            "… and {} more",
            paths.len() - MAX_COMPLETION_FILE_CHANGES
        ));
    }
    out.join(", ")
}

/// Record a spawned worker's completion as a delivery to its spawner. The mailbox
/// is resolved only through `mailbox_for_actor(root, reply_to, correlation)`, the
/// same safe path used by delivery routing, so the worker cannot direct a
/// kernel-authored event to an arbitrary raw topic. The event is stamped with
/// `sender = <worker_session>` and carries the spawn correlation so the spawner's
/// resumed turn remains tied to the original dispatch.
fn emit_completion_delivery(
    root: &Root,
    worker_session: &str,
    reply_to: &str,
    correlation: Option<&str>,
    status: Option<&std::process::ExitStatus>,
    summary: &CaptureSummary,
    launch_error: Option<&str>,
) -> Result<i64> {
    let mailbox = mailbox_for_actor(root, reply_to, correlation).with_context(|| {
        format!(
            "resolving completion mailbox for reply_to {reply_to:?} \
             (worker {worker_session})"
        )
    })?;
    let prompt = completion_delivery_prompt(worker_session, status, summary, launch_error);
    let conn = crate::db::open(root).context("opening the ledger to record the completion")?;
    crate::db::init_schema(&conn)?;
    crate::events::emit(
        root,
        &conn,
        crate::events::EmitOpts {
            payload: Some(json!({ "prompt": prompt })),
            correlation: correlation.map(str::to_string),
            sender: Some(worker_session.to_string()),
            ..crate::events::EmitOpts::new(&mailbox)
        },
    )
    .context("recording the spawned-worker completion on the ledger")
}

// ── The launch-envelope briefing (M4-B) ───────────────────────────────────────
//
// A coding agent does not, on its own, know it is running under elanus, that it may
// be resumed headlessly, or how hand-off works (docs/handoffs/coding-agents.md,
// "elanus briefs the session on the envelope at launch"). The launcher injects a
// one-time operating-envelope briefing at launch — CC via `--append-system-prompt`
// (the out-of-band system layer), Codex by prepending it to the prompt (Codex exec
// has no system-prompt flag). The per-turn ongoing context (inbox status, claims)
// is M3's separate injection seam.

/// The operating-envelope briefing text, parameterized with this session's own id
/// so the agent knows its handle. Deliberately short — it tells the agent the
/// things it can't infer: it runs under elanus; how to create or drive a worker;
/// the two dispatch modes (blocking foreground vs async wake-later); where to ask
/// for the complete verb list; and that it should behave normally toward its
/// human.
fn briefing(session: &str) -> String {
    format!(
        "You are coding session `{session}` under elanus supervision \
(an orchestration layer around you).\n\
\n\
- To create a Codex or opencode worker, run `elanus code codex \"<prompt>\"` or \
`elanus code opencode \"<prompt>\"`; the prompt is a \
positional argument. Async dispatch: use \
`elanus code deliver <worker-session> \"<message>\"` or \
`elanus code spawn <tool> \"<task>\"`. `elanus code help` lists every verb.\n\
- Dispatch modes: if you are live/interactive, run a worker in the foreground and \
read its command output. For async dispatch, use `elanus code deliver` or \
`elanus code spawn <tool> \"<task>\"`, then END YOUR TURN cleanly — do NOT poll, \
sleep, or wait; elanus wakes you later with the result.\n\
- Things addressed to you arrive as a resumed turn with the content in your prompt; \
you can also pull your own inbox with `elanus code inbox` (only YOUR mailbox). Each \
turn elanus injects an `[elanus]` note with your inbox status and any memory note. \
Prior session activity is on the bus under `obs/agent/<noun>/<session>/`.\n\
- Otherwise behave exactly as you normally would toward your human, who may or may \
not be watching this session live."
    )
}

/// Write the `/elanus` Claude Code skill into a dedicated session scratch
/// subroot, in the exact `.claude/skills/<name>/SKILL.md` shape that
/// `--add-dir <skillroot>` discovers. Keeping the added dir under `skillroot`
/// means Claude can see only the generated skill, not the sibling settings.json.
/// The launch cleanup still removes the whole scratch directory.
fn write_elanus_skill(scratch: &Path) -> Result<PathBuf> {
    let skill_root = scratch.join("skillroot");
    let skill_dir = skill_root.join(".claude").join("skills").join("elanus");
    std::fs::create_dir_all(&skill_dir)
        .with_context(|| format!("creating elanus skill dir {}", skill_dir.display()))?;
    let skill_path = skill_dir.join("SKILL.md");
    std::fs::write(&skill_path, ELANUS_SKILL)
        .with_context(|| format!("writing {}", skill_path.display()))?;
    Ok(skill_root)
}

/// Should the launch-envelope briefing be injected? Default yes; a `--no-brief`
/// flag anywhere in the user args suppresses it (and is stripped before the args
/// reach the tool, so it never confuses the binary). Returns the filtered args.
fn take_brief_flag(args: &[String]) -> (bool, Vec<String>) {
    let mut brief = true;
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        if a == "--no-brief" {
            brief = false;
        } else {
            out.push(a.clone());
        }
    }
    (brief, out)
}

/// Take the `--room <id>` launch flag (M5) out of the user args: a session
/// launched with `--room <id>` joins that coordination room, so it sees its
/// roommates' advisory edit claims in its per-turn injection and its own claims
/// surface to them. The flag (and its value) are stripped before the args reach
/// the tool, so the coding binary never sees them. Returns `(room, filtered_args)`;
/// `room` is None when no `--room` was given (a solo session — no peers, no
/// coordination). A trailing `--room` with no value is ignored (no room).
fn take_room_flag(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut room = None;
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--room" {
            // The next token is the room id; skip both. A bare trailing --room
            // (no value) is simply dropped.
            if let Some(v) = args.get(i + 1) {
                let v = v.trim();
                if !v.is_empty() {
                    room = Some(v.to_string());
                }
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        out.push(args[i].clone());
        i += 1;
    }
    (room, out)
}

/// Should Claude Code launch in headless worker mode? A `--worker` flag anywhere
/// in the user args selects `claude -p`, captures stdout, and prints a marked
/// result for a parent agent to read. The flag is stripped before the real tool
/// sees argv, matching the other elanus-only launch flags.
fn take_worker_flag(args: &[String]) -> (bool, Vec<String>) {
    let mut worker = false;
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        if a == "--worker" {
            worker = true;
        } else {
            out.push(a.clone());
        }
    }
    (worker, out)
}

/// Best-effort model / reasoning-effort metadata from explicit launch flags
/// only. The launcher cannot see the coding tool's configured defaults, so absent
/// flags are recorded as null rather than guessed.
fn extract_model_effort(args: &[String]) -> (Option<String>, Option<String>) {
    let mut model = None;
    let mut effort = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-m" | "--model" => {
                if let Some(v) = args.get(i + 1).filter(|v| !v.is_empty()) {
                    model = Some(v.clone());
                }
                i += 2;
                continue;
            }
            "--effort" => {
                if let Some(v) = args.get(i + 1).filter(|v| !v.is_empty()) {
                    effort = Some(v.clone());
                }
                i += 2;
                continue;
            }
            "-c" | "--config" => {
                if let Some(v) = args.get(i + 1) {
                    if let Some(e) = extract_reasoning_effort_config(v) {
                        effort = Some(e);
                    }
                }
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    (model, effort)
}

/// Pull `model_reasoning_effort=<value>` out of one tool config arg. The value is
/// stopped at common separators so a compound config string still yields the
/// requested scalar signal.
fn extract_reasoning_effort_config(config: &str) -> Option<String> {
    let (_, rest) = config.split_once("model_reasoning_effort=")?;
    let value = rest
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// The Codex briefing block written to the child's stdin. Codex `exec` documents:
/// "If stdin is piped and a prompt is also provided, stdin is appended as a
/// `<stdin>` block." So piping the briefing delivers it to the agent robustly,
/// WITHOUT parsing the arg list to find the prompt positional (which would be
/// fragile against flag values like `-m <model>`). The user's prompt stays the
/// positional; the briefing arrives as out-of-band context.
fn codex_briefing_block(brief: &str) -> String {
    format!("[elanus operating envelope — read before acting]\n{brief}\n")
}

/// Heuristically decide whether the `codex exec` argv already carries a prompt
/// positional. Codex accepts many flags before the prompt; we do NOT attempt a
/// full Codex CLI parse here. Instead, skip values for the common value-taking
/// flags elanus may pass through, honor `--`, and treat the first remaining
/// non-flag token as the prompt. This is intentionally conservative enough to
/// distinguish "no non-flag prompt was supplied" from the normal
/// `elanus code codex "<task>"` launch shape without baking Codex's full clap
/// grammar into elanus.
fn codex_args_have_prompt(args: &[String]) -> bool {
    let value_flags = [
        "-c",
        "--config",
        "-m",
        "--model",
        "--model-provider",
        "-s",
        "--sandbox",
        "-a",
        "--ask-for-approval",
        "--approval-policy",
        "-C",
        "--cd",
        "--profile",
    ];
    let mut after_dash_dash = false;
    let mut skip_next_value = false;
    for arg in args {
        if skip_next_value {
            skip_next_value = false;
            continue;
        }
        if after_dash_dash {
            return true;
        }
        if arg == "--" {
            after_dash_dash = true;
            continue;
        }
        if value_flags.iter().any(|flag| arg == flag) {
            skip_next_value = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return true;
    }
    false
}

/// If Codex was launched without a prompt positional, promote the launcher's own
/// stdin to that positional prompt. This keeps Codex's stdin reserved for the
/// elanus briefing block: the user prompt always reaches Codex as argv, and the
/// briefing still arrives as piped context. A terminal stdin is treated as absent
/// so `elanus code codex` fails promptly instead of waiting forever.
fn codex_args_with_prompt_from_stdin(args: &[String]) -> Result<Vec<String>> {
    if codex_args_have_prompt(args) {
        return Ok(args.to_vec());
    }

    use std::io::IsTerminal as _;
    let mut prompt = String::new();
    if !std::io::stdin().is_terminal() {
        std::io::stdin()
            .read_to_string(&mut prompt)
            .context("reading the codex prompt from stdin")?;
    }
    if prompt.trim().is_empty() {
        bail!(
            "no prompt provided: pass it as an argument (elanus code codex \"<task>\") \
             or pipe it on stdin"
        );
    }

    let mut out = args.to_vec();
    out.push(prompt);
    Ok(out)
}

// ── M3: per-turn context injection + the session's own inbox read ─────────────
//
// M3 is the PER-TURN counterpart to the one-time launch briefing: it keeps a
// session informed every turn of its inbox status and an optional memory note,
// injected OUT OF BAND (a system-reminder, after the cached prefix — not the user
// message). It is also the first increment where a session gains any READ
// capability, and that capability is deliberately narrow: a session may read ONLY
// its OWN inbox.
//
// **The read-scope crux — a scoped ledger query, NOT a bus-token widening.** The
// inbox read is `codesession::inbox_for_session`, a SQL query of the `events`
// ledger filtered to the session's OWN mailbox `in/agent/<noun>/<session>`, where
// `<noun>`/`<session>` come from the running session's own env (the launcher set
// ELANUS_CODE_AGENT / ELANUS_CODE_SESSION) — NEVER from an argument. So a session
// cannot name another session's inbox: the mailbox topic is built from its own
// identity, exactly as `elanus code hook` publishes as itself. The emit-only bus
// token's subscribe scope is UNTOUCHED (still empty — `codesession::SessionToken`
// `subscribe: Vec::new()`); the session still cannot read the bus at all. The new
// read authority is the kernel-side query gated by the env-derived identity, the
// approach docs/handoffs/coding-agents.md M3 prefers.

/// `elanus code inbox` — list THIS session's own inbox (run from inside a
/// session). Reads ELANUS_CODE_SESSION / ELANUS_CODE_AGENT from the env the
/// launcher set; the inbox is its OWN mailbox by construction (no session-id arg,
/// so it can never name another session's inbox). Prints the pending/unseen
/// deliveries (message + who-from + correlation) and marks them seen so a second
/// pull is idempotent. With `--all`, shows the full inbox (seen + unseen) and
/// marks nothing. With `--json`, emits machine-readable JSON for a tool to parse.
pub fn inbox_cmd(root: &Root, args: &[String]) -> Result<()> {
    let want_all = args.iter().any(|a| a == "--all");
    let want_json = args.iter().any(|a| a == "--json");

    // Identity comes ONLY from the env the launcher set — never an argument. This
    // is the structural own-inbox-only guarantee: there is no code path by which a
    // caller names a different session's mailbox.
    let session = std::env::var(ENV_SESSION).ok().filter(|s| !s.is_empty());
    let agent = std::env::var(ENV_AGENT).ok().filter(|s| !s.is_empty());
    let (Some(session), Some(agent)) = (session, agent) else {
        bail!(
            "elanus code inbox must run inside a coding session \
             (no {ENV_SESSION}/{ENV_AGENT} in the environment — run it from a \
             session launched by `elanus code`)"
        );
    };

    // unseen-only is the default (the interactive pull); --all shows everything.
    let items = codesession::inbox_for_session(root, &agent, &session, !want_all)?;

    if want_json {
        let arr: Vec<Value> = items
            .iter()
            .map(|it| {
                json!({
                    "event_id": it.event_id,
                    "from": it.from,
                    "correlation": it.correlation,
                    "state": it.state,
                    "created_at": it.created_at,
                    "seen": it.seen,
                    "message": it.message,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json!(arr))?);
    } else if items.is_empty() {
        println!(
            "(your inbox is empty{})",
            if want_all {
                ""
            } else {
                " — no unread messages"
            }
        );
    } else {
        println!(
            "{} message(s) in your inbox{}:",
            items.len(),
            if want_all { "" } else { " (unread)" }
        );
        for it in &items {
            let from = it.from.as_deref().unwrap_or("?");
            let corr = it
                .correlation
                .as_deref()
                .map(|c| format!(" [{c}]"))
                .unwrap_or_default();
            println!(
                "  • from {from}{corr} (event {}): {}",
                it.event_id,
                clip(&it.message, 2000)
            );
        }
    }

    // Mark the listed unseen deliveries as seen (the default pull). --all is a
    // non-destructive view: it does not change the seen-set. Idempotent either way.
    if !want_all {
        let ids: Vec<i64> = items
            .iter()
            .filter(|it| !it.seen)
            .map(|it| it.event_id)
            .collect();
        codesession::mark_inbox_seen(root, &session, &ids)?;
    }
    Ok(())
}

// ── M5: advisory edit claims (run inside a session) ───────────────────────────
//
// `elanus code claim <path>` / `elanus code unclaim <path>` record/clear an
// advisory claim that THIS session is editing <path>, visible to its roommates in
// their per-turn injection. Identity (session + agent) comes from the env the
// launcher set — never an argument — and the room from the session's own durable
// record, so a session can only ever claim AS ITSELF, IN ITS OWN ROOM. There is no
// authorization here: a claim is advisory metadata its peers read to route around
// conflicts, never a lock (recording one blocks no one).

// ── SA1: the workdir IS the room (ambient claims, no flag) ────────────────────
//
// docs/handoffs/sibling-awareness.md SA1. Two of the owner's agents in the SAME
// checkout should see each other with ZERO flags. We make that structural:
// absent an explicit `--room <id>`, a session's room defaults to a STABLE id
// derived from its CANONICAL workdir. Same checkout → same id → same room →
// siblings coordinate on turn one. An explicit `--room` still OVERRIDES (e.g. a
// planner grouping workers across directories). A solo session in a unique dir
// gets a room with no peers — identical to today, so the solo case never
// regresses. This is advisory coordination, never authorization (homogeneous
// authority, docs/security.md): the room is just the scope a session reads its
// roommates' claims from; nothing is locked, no agent is blocked by a sibling.

/// Canonicalize a workdir to a stable absolute path, falling back to the lexical
/// path when canonicalize fails (a dir that was removed, or a permission quirk) so
/// the derived room id is still deterministic for the same input. Two sessions in
/// the same checkout resolve to the same canonical path → the same room.
fn canonical_workdir(workdir: &Path) -> PathBuf {
    std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf())
}

/// Derive the stable default room id for a canonical workdir. The id only has to
/// be stable for the SAME path within one elanus install (it scopes a ledger
/// query, never a bus topic), and short + string-safe. We hash the canonical path
/// with a deterministic FNV-1a (NOT DefaultHasher — its hashing is not guaranteed
/// stable across toolchains, and two concurrently-running sessions must agree) and
/// prefix `wd-` so a workdir-room is visibly distinct from an explicit `--room`.
fn workdir_room_id(canonical: &Path) -> String {
    // FNV-1a over the path bytes — deterministic across processes and builds.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canonical.as_os_str().to_string_lossy().as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("wd-{hash:016x}")
}

/// The room a session belongs to, given its launch `--room` (if any) and its
/// workdir. An explicit room wins; otherwise default to the workdir-derived room
/// (SA1). Always Some — every session is in a room now (a solo session's room
/// simply has no peers).
fn resolve_room(explicit: Option<&str>, workdir: &Path) -> String {
    match explicit {
        Some(r) if !r.trim().is_empty() => r.trim().to_string(),
        _ => workdir_room_id(&canonical_workdir(workdir)),
    }
}

/// Resolve the running session's identity (session + agent noun) and its
/// coordination room from the env the launcher set plus its durable record. Errors
/// cleanly when run outside a coding session. SA1: a session no longer needs a
/// `--room` flag — when its record carries no explicit room it derives the default
/// workdir-room from its recorded workdir, so `claim`/`unclaim`/`claims` work in
/// the same checkout with no flags. The room is the session's own (read from the
/// record or derived from its own workdir), never supplied by the caller.
fn session_room_identity(root: &Root) -> Result<(String, String)> {
    let session = std::env::var(ENV_SESSION).ok().filter(|s| !s.is_empty());
    let Some(session) = session else {
        bail!(
            "this command must run inside a coding session \
             (no {ENV_SESSION} in the environment — run it from a session launched \
             by `elanus code`)"
        );
    };
    let rec = codesession::read_record(root, &session)
        .context("reading this session's record")?
        .with_context(|| {
            format!(
                "no record for session {session:?} yet \
                 (its native session id may not be observed — try again after the \
                 first turn)"
            )
        })?;
    // SA1: an absent/empty room is no longer an error — derive the default
    // workdir-room from the session's own recorded workdir, so siblings in the
    // same checkout coordinate with zero flags. An explicit room on the record
    // (set at launch from `--room`) still wins.
    let room = match rec.room.filter(|r| !r.is_empty()) {
        Some(r) => r,
        None => resolve_room(None, Path::new(&rec.workdir)),
    };
    Ok((session, room))
}

/// `elanus code claim <path>` — announce that THIS session is editing <path>
/// (advisory; visible to roommates, locks nothing). Identity + room are derived
/// from the running session, never an argument. Re-claiming the same path is
/// idempotent.
pub fn claim_cmd(root: &Root, path: &str) -> Result<()> {
    let path = path.trim();
    if path.is_empty() {
        bail!("usage: elanus code claim <path>");
    }
    let (session, room) = session_room_identity(root)?;
    codesession::add_claim(root, &room, &session, path)?;
    println!(
        "claimed {path} in room {room} (advisory — your peers will see you are \
editing it; nothing is locked)"
    );
    Ok(())
}

/// `elanus code unclaim <path>` — release THIS session's advisory claim on <path>
/// (e.g. when it has finished). Only the session's OWN claim is cleared; it can
/// never clear a peer's. Idempotent (unclaiming a path it doesn't hold is a no-op).
pub fn unclaim_cmd(root: &Root, path: &str) -> Result<()> {
    let path = path.trim();
    if path.is_empty() {
        bail!("usage: elanus code unclaim <path>");
    }
    let (session, room) = session_room_identity(root)?;
    let removed = codesession::remove_claim(root, &room, &session, path)?;
    if removed {
        println!("released your claim on {path} in room {room}");
    } else {
        println!("(you held no claim on {path} in room {room})");
    }
    Ok(())
}

/// `elanus code claims [--json]` — show what THIS session sees in its room: its
/// own claims and its peers' (the advisory coordination view). Read-only.
pub fn claims_cmd(root: &Root, args: &[String]) -> Result<()> {
    let want_json = args.iter().any(|a| a == "--json");
    let (session, room) = session_room_identity(root)?;
    let own = codesession::own_claims(root, &room, &session)?;
    let peers = codesession::peer_claims(root, &room, &session)?;
    if want_json {
        let to_json = |c: &codesession::Claim| json!({ "session": c.session, "path": c.path, "created_at": c.created_at });
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "room": room,
                "session": session,
                "own": own.iter().map(to_json).collect::<Vec<_>>(),
                "peers": peers.iter().map(to_json).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }
    println!("room {room} — your session {session}");
    if own.is_empty() {
        println!("  you hold no claims");
    } else {
        println!("  your claims:");
        for c in &own {
            println!("    editing {}", c.path);
        }
    }
    if peers.is_empty() {
        println!("  no peer claims (no roommates are editing anything)");
    } else {
        println!("  peer claims (advisory — route around these):");
        for c in &peers {
            println!("    {} is editing {}", c.session, c.path);
        }
    }
    Ok(())
}

/// `elanus code note <session> "<text>"` — set (or replace) a session's memory
/// note, surfaced by the per-turn injection. An empty `<text>` clears the note.
/// Run by a planner (or a human) to leave a worker a persistent reminder. Unlike
/// `inbox`, this names the target session explicitly (a planner annotates a
/// worker) — and it is plumbing + record, not a bus authority: it writes a row a
/// session reads back through its own per-turn injection, with no token involved.
pub fn note_cmd(root: &Root, session: &str, text: &str) -> Result<()> {
    let session = session.trim();
    if session.is_empty() {
        bail!("usage: elanus code note <session> \"<text>\"  (empty text clears the note)");
    }
    // A note can only attach to a real recorded session — otherwise it would sit
    // unread (nothing surfaces it). Keep the failure honest.
    if codesession::read_record(root, session)?.is_none() {
        bail!(
            "no coding session {session:?} to leave a note for \
             (never launched, or its native session id was never observed)"
        );
    }
    codesession::set_note(root, session, text)?;
    if text.trim().is_empty() {
        println!("cleared the note for {session}");
    } else {
        println!("note set for {session}");
    }
    Ok(())
}

/// Build the per-turn injection text for a session — the OUT-OF-BAND system note
/// (system-reminder layer for CC, the `[elanus]` resume block for codex) that
/// reports the session's current inbox status and any memory note. Returns None
/// when there is nothing to say (no unseen inbox, no note) so a quiet turn injects
/// nothing. Deliberately short; it is per-turn context, kept OUT of the cached
/// prefix (it changes every turn) so it never busts prompt caching.
///
/// The inbox read is the same own-inbox-only scoped query the CLI uses — built
/// from the session's own `agent_noun`/`session`, never a caller-supplied id.
pub fn turn_injection(root: &Root, agent_noun: &str, session: &str) -> Option<String> {
    let unseen = codesession::inbox_for_session(root, agent_noun, session, true)
        .ok()
        .unwrap_or_default();
    let note = codesession::get_note(root, session).ok().flatten();

    // M5/SA1: the session's roommates' current advisory edit claims (excluding its
    // own). The room comes from the session's OWN durable record — never an
    // argument. SA1: when the record carries no explicit room, derive the default
    // workdir-room from the recorded workdir, so siblings in the same checkout see
    // each other with zero flags. A genuinely solo session's room simply has no
    // peers.
    let rec = codesession::read_record(root, session).ok().flatten();
    let workdir = rec.as_ref().map(|r| r.workdir.clone()).unwrap_or_default();
    let room = rec
        .as_ref()
        .and_then(|r| r.room.clone())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| {
            if workdir.is_empty() {
                String::new()
            } else {
                resolve_room(None, Path::new(&workdir))
            }
        });
    let peer_claims = if room.is_empty() {
        Vec::new()
    } else {
        codesession::peer_claims(root, &room, session)
            .ok()
            .unwrap_or_default()
    };

    // SA2: the LIVE siblings sharing this session's workdir (roster + liveness from
    // `code_sessions`, honest liveness — stale/dead sessions age out). "What each
    // is touching" is cross-referenced from SA1's per-path claims below (per-file
    // touch is not projected — docs/handoffs/sibling-awareness.md data note). A
    // solo session has no live siblings, so this stays empty and the turn is quiet.
    //
    // SA3 (touching-a-file IS the claim — an obs/fs bus subscriber that auto-creates
    // claims) is DEFERRED: it is a new long-running runtime component whose read
    // half rides the not-yet-shipped read camera, out of scope for this read-only
    // injection change. Until it lands, "what each sibling is touching" is only as
    // fresh as the claims an agent volunteered via `elanus code claim`.
    let live_siblings = if workdir.is_empty() {
        Vec::new()
    } else {
        codesession::live_siblings(root, session, &workdir)
    };

    if unseen.is_empty() && note.is_none() && peer_claims.is_empty() && live_siblings.is_empty() {
        return None;
    }

    let mut out = String::new();

    // SA2: PREPEND one line naming the live siblings in this workdir, and — where a
    // claim tells us — what each is touching. One line (count + the one or two most
    // relevant siblings, most-recently-active first), so a busy directory never
    // floods the turn. Quiet when alone: this block is absent for a solo session.
    if !live_siblings.is_empty() {
        // Map each sibling to its most recent claimed path (if any) so we can say
        // "last editing <path>". peer_claims is ordered oldest→newest, so the last
        // match wins. Per-file touch is only as fresh as SA1's claims (SA3 deferred).
        let mut last_path: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for c in &peer_claims {
            last_path.insert(c.session.as_str(), c.path.as_str());
        }
        const MAX_NAMED: usize = 2;
        let n = live_siblings.len();
        out.push_str(&format!(
            "[elanus siblings] {n} other coding session(s) active here (advisory — \
divide the work, nothing is locked): "
        ));
        let named: Vec<String> = live_siblings
            .iter()
            .take(MAX_NAMED)
            .map(|s| {
                let touching = last_path
                    .get(s.session.as_str())
                    .map(|p| format!(", last editing {}", clip(p, 200)))
                    .unwrap_or_default();
                format!("{} ({}){}", s.session, s.agent_noun, touching)
            })
            .collect();
        out.push_str(&named.join("; "));
        if n > MAX_NAMED {
            out.push_str(&format!("; …and {} more", n - MAX_NAMED));
        }
        // SA4: a sibling shares this WORKING TREE → suggest isolating with a git
        // worktree (advisory; never auto-creates, never blocks — homogeneous
        // authority). Scoped to "same canonical workdir" (live_siblings already
        // matched on it). TODO(SA4): when both sessions are distinct *worktrees* of
        // one repo they share no index to collide on — skip the nudge there by
        // comparing `git rev-parse --git-common-dir` / worktree paths. Distinguishing
        // worktrees is non-trivial (needs a git probe), so it is deferred per the
        // handoff's "scope to same canonical workdir" fallback.
        out.push_str(
            "\n  You share this working tree with the session(s) above; if you will edit \
overlapping files, consider isolating in a separate `git worktree` to avoid a shared-index \
collision (advisory).",
        );
        out.push('\n');
    }

    out.push_str("[elanus] ");
    if unseen.is_empty() {
        out.push_str("Your inbox has no new messages.");
    } else {
        out.push_str(&format!(
            "You have {} new message(s) in your inbox. Run `elanus code inbox` to read them.",
            unseen.len()
        ));
        // A brief preview of the most recent one or two (clipped), so the agent
        // has a hint without pulling — but the authoritative read is the command.
        if let Some(latest) = unseen.last() {
            let from = latest.from.as_deref().unwrap_or("?");
            out.push_str(&format!(
                "\n  Latest from {from}: {}",
                clip(&latest.message, 200)
            ));
        }
    }
    if let Some(note) = note {
        out.push_str(&format!("\n[elanus note] {}", clip(&note, 2000)));
    }
    // M5: surface peers' claims as advisory routing info — "code-X is editing
    // src/foo.rs" — so this session can route around them. Advisory only; nothing
    // is locked. One line per claim (capped so a busy room can't flood the turn).
    if !peer_claims.is_empty() {
        out.push_str(&format!(
            "\n[elanus peers] {} other session(s) in room {room} have active edit claims \
(advisory — route around these files, nothing is locked):",
            peer_claims
                .iter()
                .map(|c| c.session.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        ));
        const MAX_CLAIMS: usize = 50;
        for c in peer_claims.iter().take(MAX_CLAIMS) {
            out.push_str(&format!(
                "\n  {} is editing {}",
                c.session,
                clip(&c.path, 400)
            ));
        }
        if peer_claims.len() > MAX_CLAIMS {
            out.push_str(&format!("\n  …and {} more", peer_claims.len() - MAX_CLAIMS));
        }
    }
    Some(out)
}

/// Detect whether the current Claude Code `UserPromptSubmit` payload looks like
/// the user is asking about delegation, subagents, parallel work, or Codex, so
/// the hook can add a focused per-turn dispatch hint exactly when it is relevant.
fn user_prompt_mentions_dispatch(payload: &serde_json::Value) -> bool {
    let Some(prompt) = payload.get("prompt").and_then(Value::as_str) else {
        return false;
    };
    let prompt = prompt.to_lowercase();
    [
        "subagent",
        "sub-agent",
        "sub agent",
        "delegate",
        "delegating",
        "dispatch",
        "spawn",
        "in parallel",
        "worker",
        "another agent",
        "codex",
    ]
    .iter()
    .any(|needle| prompt.contains(needle))
}

/// Compose the message a driven resume hands the model: the per-turn `[elanus]`
/// injection block (inbox status + memory note) prepended to the delivered
/// message, OUT OF BAND. The injection is bracketed so the model reads it as a
/// system note, not as the user's words; the delivered message follows under its
/// own marker. When there is nothing to inject (a quiet turn — no unseen inbox, no
/// note), the message is returned unchanged, so a plain resume stays plain.
fn build_resume_message(root: &Root, agent_noun: &str, session: &str, message: &str) -> String {
    match turn_injection(root, agent_noun, session) {
        Some(ctx) => {
            format!("{ctx}\n[elanus] The message you were resumed with follows.\n\n{message}")
        }
        None => message.to_string(),
    }
}

/// `elanus code <tool> [args...]` — launch the real coding agent, observed.
pub fn launch(root: &Root, tool: &str, args: &[String]) -> Result<()> {
    // If this launcher is itself running inside a coding session, capture that
    // parent edge before this function sets ENV_SESSION for the child session.
    // A blocking nested launch inherits the parent in ENV_SESSION; a DETACHED
    // `spawn` worker has ENV_SESSION scrubbed (it mints its own identity) but
    // carries its spawner in ENV_REPLY_TO — so fall back to that, otherwise a
    // spawned worker would lose the parent→child edge the session tree needs.
    let parent = std::env::var(ENV_SESSION)
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var(ENV_REPLY_TO).ok().filter(|s| !s.is_empty()));

    // Reap any session tokens a prior SIGKILL'd launcher leaked, before anything
    // else — a crash must never leave a usable credential lying around
    // (docs/security.md). Done first (even before tool parsing) so a launch is
    // an opportunity to heal orphans regardless of how it turns out. Daemon boot
    // does the same sweep; doing it here too means a launcher heals orphans even
    // against a never-restarted daemon.
    for orphan in codesession::reap_orphans(root) {
        eprintln!("[code] reaped orphaned session credential {orphan}");
    }
    // M5: also reap any room membership + claims a SIGKILL'd session leaked, so a
    // dead session's advisory claims don't linger in its roommates' injections.
    for (room, sess) in codesession::reap_dead_members(root) {
        eprintln!("[code] reaped claims of dead session {sess} in room {room}");
    }

    // The launch-envelope briefing rides a launch flag (default on; `--no-brief`
    // suppresses it). The coordination room rides `--room <id>` (M5). Claude's
    // worker shape rides `--worker`. Strip all before the args reach the tool.
    let (want_brief, args) = take_brief_flag(args);
    let (room, args) = take_room_flag(&args);
    let (worker, args) = take_worker_flag(&args);
    let args = &args[..];
    let (model, effort) = extract_model_effort(args);
    let worker_timeout = std::env::var(ENV_REPLY_TO)
        .ok()
        .filter(|s| !s.is_empty())
        .map(|_| spawn_timeout_secs());

    let tool = Tool::parse(tool)?;
    let session = launch_session_id(root);
    let agent = tool.agent_noun().to_string();
    let brief_text = want_brief.then(|| briefing(&session));

    // Per-session identity: a GRANT-SCOPED session token (NOT a full-authority
    // fenced secret — docs/security.md entry 16). The launcher is uncaged (the
    // human ran it), so it can place the token in the fenced store; that is what
    // lets the session's hook bridge authenticate as ITSELF and the broker stamp
    // the session — not the owner — as the sender, while holding it to its own
    // obs subtree. We record this launcher's pid as the token owner so the reaper
    // can distinguish a live session from a SIGKILL orphan.
    let principal = session.clone();
    // M1 (authority-delegation): pass the spawner session so mint can enforce
    // Σ children ≤ parent.remaining at the fenced-store level (never from env).
    // `parent` is None when the owner runs `elanus code` directly → unbounded.
    // No explicit budget request here — inherit-equal is the default policy.
    let token = codesession::mint(root, &principal, &agent, std::process::id() as i32,
                                  parent.as_deref(), None)
        .with_context(|| format!("minting the session credential for {principal}"))?;
    let bus_token = token.secret.clone();

    // SA1: every session is in a room now. An explicit `--room <id>` wins;
    // otherwise the room defaults to a stable id derived from the CANONICAL
    // workdir, so two `elanus code` launched in the SAME checkout with NO flags
    // see each other (docs/handoffs/sibling-awareness.md). This is advisory
    // coordination, not authorization — a solo session in a unique dir gets a room
    // with no peers, identical to today.
    //
    // We compute the workdir HERE (the launcher's cwd, the dir the child runs in)
    // so the derived room matches what `session_room_identity` later derives from
    // the recorded workdir. The native session id isn't known yet, so set_room
    // writes/stubs the record's room and the later native-id upsert preserves it
    // (COALESCE). Best-effort: a room-setup failure must not break the launch.
    let launch_workdir = std::env::current_dir().unwrap_or_else(|_| root.dir.clone());
    let room = resolve_room(room.as_deref(), &launch_workdir);
    {
        if let Err(e) = codesession::set_room(root, &session, &room) {
            eprintln!("[code] setting room {room} (continuing without coordination): {e:#}");
        }
        if let Err(e) =
            codesession::join_room(root, &room, &session, &agent, std::process::id() as i32)
        {
            eprintln!("[code] joining room {room} (continuing without coordination): {e:#}");
        } else {
            eprintln!("[code] session {session} joined coordination room {room}");
        }
    }

    // The session's run scratch — for CC, the generated hook config lives here;
    // for Codex (no hooks) it's still created for symmetry and is empty. Never
    // ~/.claude / ~/.codex.
    let scratch = root.run_dir().join(&session);
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("creating run scratch {}", scratch.display()))?;
    let settings_path = scratch.join("settings.json");

    let self_exe =
        std::env::current_exe().context("locating the elanus binary for hook commands")?;
    let result = (|| -> Result<(std::process::ExitStatus, CaptureSummary)> {
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
                "parent": parent,
                "model": model,
                "effort": effort,
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
                let skill_root = write_elanus_skill(&scratch)?;

                // Launch the real binary with the generated, isolated config. The
                // TUI gets inherited stdio so it is a normal, fully usable
                // session. `--setting-sources ''` loads NO user/project/local
                // settings (the user's ~/.claude hooks/CLAUDE.md are untouched);
                // `--settings <file>` loads only our generated hooks (Appendix A).
                if worker {
                    let mut tool_args = vec![
                        "--settings".to_string(),
                        settings_path.display().to_string(),
                        "--setting-sources".to_string(),
                        "".to_string(),
                        "--add-dir".to_string(),
                        skill_root.display().to_string(),
                    ];
                    if let Some(brief) = &brief_text {
                        tool_args.push("--append-system-prompt".to_string());
                        tool_args.push(brief.clone());
                    }
                    tool_args.push("-p".to_string());
                    tool_args.extend_from_slice(args);
                    let timeout_suffix;
                    let (program, tool_args) = if let Some(secs) = worker_timeout {
                        timeout_suffix = format!(" [timeout {secs}s]");
                        timeout_wrap(tool.binary(), &tool_args, secs)
                    } else {
                        timeout_suffix = String::new();
                        (tool.binary().to_string(), tool_args)
                    };
                    let mut cmd = std::process::Command::new(&program);
                    cmd.args(&tool_args);
                    // Scrub elanus's provider credentials FIRST so Claude Code uses
                    // its own login (`~/.claude`) rather than inheriting elanus's
                    // DeepSeek ANTHROPIC_BASE_URL/API_KEY (Task 2). The ELANUS_*
                    // vars set below are NOT scrubbed — the hook bridge depends on
                    // them.
                    scrub_provider_creds(&mut cmd);
                    scrub_launch_control_env(&mut cmd);
                    // The session's own identity, carried to the hook bridge
                    // children CC spawns. ELANUS_PACKAGE + ELANUS_BUS_TOKEN are
                    // what `elanus bus pub` authenticates with (src/buscli.rs);
                    // ELANUS_CODE_* tell the bridge which session/agent to file
                    // under.
                    cmd.env("ELANUS_PACKAGE", &principal)
                        .env("ELANUS_BUS_TOKEN", &bus_token)
                        .env(ENV_SESSION, &session)
                        .env(ENV_AGENT, &agent)
                        .env("ELANUS_ROOT", &root.dir);
                    eprintln!(
                        "[code] launching {} as session {session}{timeout_suffix}",
                        tool.binary()
                    );
                    cmd.stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::inherit());
                    let output = cmd.output().with_context(|| {
                        format!("launching {program} (is it installed and on PATH?)")
                    })?;
                    let text = String::from_utf8_lossy(&output.stdout);
                    print_claude_worker_result(&session, &text);
                    let final_text = (!text.trim().is_empty()).then(|| clip(&text, FINAL_TEXT_CAP));
                    Ok((
                        output.status,
                        CaptureSummary {
                            final_text,
                            file_changes: Vec::new(),
                        },
                    ))
                } else {
                    // Foreground/interactive launches are deliberately NOT wrapped
                    // in timeout; real live delegations can run as long as needed.
                    let mut cmd = std::process::Command::new(tool.binary());
                    cmd.arg("--settings")
                        .arg(&settings_path)
                        .arg("--setting-sources")
                        .arg("")
                        .arg("--add-dir")
                        .arg(&skill_root);
                    // The launch-envelope briefing (M4-B): Claude Code injects it
                    // out-of-band via --append-system-prompt (the system layer,
                    // after the cached prefix — Appendix A), not the user message.
                    if let Some(brief) = &brief_text {
                        cmd.arg("--append-system-prompt").arg(brief);
                    }
                    // Scrub elanus's provider credentials FIRST so Claude Code uses
                    // its own login (`~/.claude`) rather than inheriting elanus's
                    // DeepSeek ANTHROPIC_BASE_URL/API_KEY (Task 2). The ELANUS_*
                    // vars set below are NOT scrubbed — the hook bridge depends on
                    // them.
                    scrub_provider_creds(&mut cmd);
                    scrub_launch_control_env(&mut cmd);
                    // The session's own identity, carried to the hook bridge
                    // children CC spawns. ELANUS_PACKAGE + ELANUS_BUS_TOKEN are
                    // what `elanus bus pub` authenticates with (src/buscli.rs);
                    // ELANUS_CODE_* tell the bridge which session/agent to file
                    // under.
                    cmd.env("ELANUS_PACKAGE", &principal)
                        .env("ELANUS_BUS_TOKEN", &bus_token)
                        .env(ENV_SESSION, &session)
                        .env(ENV_AGENT, &agent)
                        .env("ELANUS_ROOT", &root.dir);
                    eprintln!("[code] launching {} as session {session}", tool.binary());
                    cmd.args(args);
                    let status = cmd.status().with_context(|| {
                        format!("launching {} (is it installed and on PATH?)", tool.binary())
                    })?;
                    Ok((status, CaptureSummary::default()))
                }
            }
            // ── StreamJson: stdout JSONL stream ───────────────────────────────
            // No hooks. Run the tool's headless JSON mode, pipe stdout, and
            // parse+publish each event in-process as the session principal. The
            // Capture strategy is per-strategy (not per-tool), so dispatch on the
            // concrete Tool here: Codex (`exec --json`) vs opencode
            // (`run --format json`). Both fill the same envelope.
            Capture::StreamJson => match tool {
                Tool::OpenCode => run_opencode_capture(
                    root,
                    &principal,
                    &bus_token,
                    &agent,
                    &session,
                    &workdir,
                    args,
                    brief_text.as_deref(),
                    worker,
                    worker_timeout,
                ),
                // Codex (the original StreamJson tool). ClaudeCode never reaches
                // here (it is HookBridge), so the default is Codex.
                _ => run_codex_capture(
                    root,
                    &principal,
                    &bus_token,
                    &agent,
                    &session,
                    &workdir,
                    args,
                    brief_text.as_deref(),
                    worker_timeout,
                ),
            },
        }
    })();

    // Stop (the last ordered record): always emitted, even on a launch error,
    // so the bus shows the session ended and with what code.
    let exit_code = result.as_ref().ok().and_then(|(s, _)| s.code());
    publish_obs(
        root,
        &principal,
        &bus_token,
        &obs_topic(&agent, &session, "session/stop"),
        json!({ "ts": now_iso(), "exit_code": exit_code }),
    );

    // A detached spawn asks this wrapper to route the worker's result back to the
    // spawning session's mailbox. This is best-effort and uses the same safe
    // actor→mailbox resolver as delivery routing; a reply failure is logged but
    // never changes the worker's normal stop/cleanup/exit behavior.
    if let Some(reply_to) = std::env::var(ENV_REPLY_TO).ok().filter(|s| !s.is_empty()) {
        let correlation = std::env::var(ENV_REPLY_CORRELATION)
            .ok()
            .filter(|s| !s.is_empty());
        let launch_error = result.as_ref().err().map(|e| format!("{e:#}"));
        let fallback_summary;
        let (status, summary) = match result.as_ref() {
            Ok((status, summary)) => (Some(status), summary),
            Err(_) => {
                fallback_summary = CaptureSummary {
                    final_text: launch_error.as_deref().map(|e| clip(e, FINAL_TEXT_CAP)),
                    file_changes: Vec::new(),
                };
                (None, &fallback_summary)
            }
        };
        if let Err(e) = emit_completion_delivery(
            root,
            &session,
            &reply_to,
            correlation.as_deref(),
            status,
            summary,
            launch_error.as_deref(),
        ) {
            eprintln!("[code] delivering spawned-worker completion failed (continuing): {e:#}");
        }
    }

    // No home-state pollution and no lingering credential: drop the generated
    // config and retire the session's scoped token (best-effort; a SIGKILL leaves
    // it, but it is reaped at the next launcher/daemon boot, and even unreaped it
    // can only ever publish this dead session's own obs subtree — never the
    // owner, work, or another agent).
    let _ = std::fs::remove_dir_all(&scratch);
    codesession::retire(root, &principal);
    // M5: room membership + advisory claims are NOT released here. A coding
    // session is DURABLE and RESUMABLE (M2-A: a turn-process exiting is not the
    // session ending) — a one-shot `codex exec` / `claude -p` turn ends its
    // process every turn, but the session lives on and may resume editing, so its
    // claims must persist between turns (otherwise a worker would lose its claims
    // the instant it finished a turn). Release is therefore by:
    //   - explicit `elanus code unclaim <path>` (the agent finished a file), and
    //   - crash-reap: `reap_dead_members` drops the membership+claims of a session
    //     whose owner pid is gone, at the next launcher/daemon boot — so a
    //     SIGKILL'd (or simply finished) session's claims don't linger forever
    //     (the lease-released membership of docs/topics.md decided-5).
    // The owner pid recorded at join is this launcher's pid; once this process
    // exits it is dead, so the boot reaper will release the membership — but not
    // mid-turn, and not while a live interactive launcher holds the session open.

    let (status, _summary) = result?;
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
/// CC's sandbox. The user prompt must be a positional arg by the time Codex is
/// spawned; if the caller omitted one, we promote the launcher's stdin to that
/// positional or fail loudly before spawning. Codex's stdin remains reserved for
/// the elanus briefing block, and stderr is inherited so the human still sees
/// Codex's own progress/errors. Returns the native exit status plus the captured
/// legible result so foreground launches can print it and spawned launches can
/// route it back to the spawner.
#[allow(clippy::too_many_arguments)]
fn run_codex_capture(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    workdir: &Path,
    args: &[String],
    brief: Option<&str>,
    worker_timeout: Option<u64>,
) -> Result<(std::process::ExitStatus, CaptureSummary)> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let args = codex_args_with_prompt_from_stdin(args)?;

    let mut codex_args = vec![
        "exec".to_string(),
        "--json".to_string(),
        "--skip-git-repo-check".to_string(),
    ];
    codex_args.extend_from_slice(&args);
    let timeout_suffix;
    let (program, codex_args) = if let Some(secs) = worker_timeout {
        timeout_suffix = format!(" [timeout {secs}s]");
        timeout_wrap("codex", &codex_args, secs)
    } else {
        timeout_suffix = String::new();
        ("codex".to_string(), codex_args)
    };

    let mut cmd = Command::new(&program);
    cmd.args(&codex_args);
    // The launch-envelope briefing (M4-B): Codex exec has no --append-system-prompt,
    // so we deliver it on STDIN. Codex appends piped stdin as a `<stdin>` block
    // alongside the prompt positional — robust, no arg parsing. stdin is piped only
    // when there is a briefing to write; otherwise null (the prompt is the arg, so
    // the child never blocks on stdin). Piped stdout (we parse it), inherited stderr
    // (the human sees Codex's own output). We keep the real CODEX_HOME — setting it
    // to a scratch would drop the user's auth.
    cmd.stdin(if brief.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit());
    // Scrub elanus's provider credentials so Codex uses its own login (`~/.codex`)
    // rather than inheriting elanus's OPENAI_*/ANTHROPIC_* provider env (Task 2).
    // The ELANUS_* vars set below are NOT scrubbed.
    scrub_provider_creds(&mut cmd);
    scrub_launch_control_env(&mut cmd);
    cmd.env_remove("ELANUS_PACKAGE")
        .env_remove("ELANUS_BUS_TOKEN");
    // The session's own identity, carried to anything the codex session spawns —
    // crucially `elanus code deliver`, which reads ELANUS_CODE_SESSION/AGENT to
    // record the running session as the requester, and ELANUS_ROOT to resolve the
    // same root. (Bus auth uses the in-process env per publish; these are for child
    // processes the agent runs.)
    cmd.env(ENV_SESSION, session)
        .env(ENV_AGENT, agent)
        .env("ELANUS_ROOT", &root.dir);
    eprintln!("[code] launching codex exec --json as session {session}{timeout_suffix}");

    let mut child = cmd
        .spawn()
        .with_context(|| format!("launching {program} (is it installed and on PATH?)"))?;

    // Write the briefing to stdin (then close it, so codex stops reading). The
    // child also has the prompt positional; codex folds piped stdin in as context.
    if let Some(b) = brief {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(codex_briefing_block(b).as_bytes());
            // Dropping stdin here closes it → EOF, so codex proceeds.
        }
    }

    // On a fresh launch, `thread.started` carries codex's native thread id —
    // persist the durable record (with this workdir) the moment we see it so the
    // session is resumable after the launcher exits. The same capture pass also
    // harvests the worker's legible result; print it in-band for blocking
    // foreground callers (resume uses the summary for routed completion).
    let summary = capture_codex_stream(
        root,
        principal,
        bus_token,
        agent,
        session,
        &mut child,
        Some(workdir),
    );
    print_codex_worker_result(session, &summary);

    let status = child.wait().context("waiting for codex exec to finish")?;
    Ok((status, summary))
}

// ── opencode adapter (StreamJson) ─────────────────────────────────────────────
//
// OBSERVED EVENT SCHEMA — `opencode run --format json` (v1.17.9).
// Pinned 2026-06-21 from the installed binary: the run loop emits ONE JSON object
// PER LINE (JSONL) on stdout via `process.stdout.write(JSON.stringify({type,
// timestamp, sessionID, ...rest}) + "\n")`. Every line therefore has:
//   { "type": <string>, "timestamp": <ms>, "sessionID": "ses_…", …rest }
// The `sessionID` (opencode's native, resumable session id) is on EVERY event and
// is known before the first model token (it comes from the resolved session).
// The emitted `type` values + their `rest` payloads (from the run handler's
// `p(type, rest)` call sites):
//   "text"        → { part: <TextPart> }   — only when part.time.end is set (the
//                    SETTLED assistant text). The LAST one is the final answer.
//   "tool_use"    → { part: <ToolPart> }   — only when state.status is "completed"
//                    or "error". Carries `tool` (name), `callID`, and `state`
//                    ({status, input, output|error, title, metadata, time}).
//                    NOTE: a single combined event per tool (the SETTLED tool),
//                    NOT separate call/result — so we project BOTH a tool/<n>/call
//                    (from state.input) and a tool/<n>/result (from state.output).
//   "reasoning"   → { part: <ReasoningPart> } — settled reasoning text.
//   "step_start"  → { part: <StepStartPart> }   — a model step boundary.
//   "step_finish" → { part: <StepFinishPart> }  — carries tokens/cost.
//   "error"       → { error: <…> }              — a session/stream error.
// There is NO explicit "done"/"end" event: the run loop breaks on the server's
// `session.status` idle and the process exits (stdout EOF). The TERMINAL signal is
// EOF; the final answer is the last "text" event's part.text.
// Part shapes (from the server OpenAPI `/doc`, same vocabulary):
//   TextPart      { id, sessionID, messageID, type:"text", text, time:{start,end?} }
//   ToolPart      { id, sessionID, messageID, type:"tool", callID, tool, state }
//   ToolState(completed) { status:"completed", input:{}, output:"", title, time }
//   ToolState(error)     { status:"error", input:{}, error:"", time }
//   Session       { id:"ses_…", title, directory, … }
//
// DEFERRED (see the onboard-opencode handoff): OC3 — the live SSE/ServerEvents
// capture variant (opencode is client/server; `serve` exposes an SSE stream) — and
// OC5 — folding all three adapters into the HM1 `Harness` trait. This adapter is
// the headless `run --format json` path only (OC1/OC2/OC4).

/// Compose the opencode run message: opencode's `run` has no out-of-band
/// system-prompt flag, so the launch-envelope briefing rides the prompt positional
/// as a leading `[elanus]` block ahead of the task (the same shape codex uses on
/// stdin, but folded into the positional here since opencode reads its prompt as
/// argv). Returned verbatim when there is no briefing.
fn opencode_message_with_brief(brief: Option<&str>, task: &str) -> String {
    match brief {
        Some(b) => format!("[elanus operating envelope — read before acting]\n{b}\n\n{task}"),
        None => task.to_string(),
    }
}

/// Build the opencode `run` positional task from the launcher args (everything
/// after flag-stripping). Joins multiple tokens with spaces (the normal launch
/// shape is a single quoted task). If no task token was supplied, promote the
/// launcher's stdin (non-terminal) to the task — symmetric with codex — so
/// `elanus code opencode` from a pipe still works; a terminal/empty stdin fails
/// loudly rather than hanging.
fn opencode_task_from_args(args: &[String]) -> Result<String> {
    let task = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if !task.trim().is_empty() {
        return Ok(task);
    }
    use std::io::IsTerminal as _;
    let mut prompt = String::new();
    if !std::io::stdin().is_terminal() {
        std::io::stdin()
            .read_to_string(&mut prompt)
            .context("reading the opencode prompt from stdin")?;
    }
    if prompt.trim().is_empty() {
        bail!(
            "no prompt provided: pass it as an argument (elanus code opencode \"<task>\") \
             or pipe it on stdin"
        );
    }
    Ok(prompt)
}

/// Launch `opencode run --format json` headless, pipe its JSONL stdout, and
/// parse+publish each event in-process as the session principal (the StreamJson
/// envelope, mirroring `run_codex_capture`). `--pure` runs without external plugins
/// (the analog of Claude's `--setting-sources ''`); `--dangerously-skip-permissions`
/// is opencode's headless auto-approve (a worker can't answer interactive prompts) —
/// it is passed for worker/headless launches. opencode brings its OWN provider auth
/// (`opencode auth`), so we scrub elanus's provider creds (PROVIDER_CRED_VARS) — none
/// of which opencode reads — leaving its own login intact.
#[allow(clippy::too_many_arguments)]
fn run_opencode_capture(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    workdir: &Path,
    args: &[String],
    brief: Option<&str>,
    worker: bool,
    worker_timeout: Option<u64>,
) -> Result<(std::process::ExitStatus, CaptureSummary)> {
    use std::process::{Command, Stdio};

    let task = opencode_task_from_args(args)?;
    let message = opencode_message_with_brief(brief, &task);

    let mut oc_args = vec![
        "run".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--pure".to_string(),
    ];
    // Headless auto-approve: a captured worker can't answer interactive permission
    // prompts. Passed for worker/headless launches (opencode keeps its OWN sandbox
    // active either way — this only skips the interactive gate).
    if worker {
        oc_args.push("--dangerously-skip-permissions".to_string());
    }
    oc_args.push(message);

    let timeout_suffix;
    let (program, oc_args) = if let Some(secs) = worker_timeout {
        timeout_suffix = format!(" [timeout {secs}s]");
        timeout_wrap("opencode", &oc_args, secs)
    } else {
        timeout_suffix = String::new();
        ("opencode".to_string(), oc_args)
    };

    let mut cmd = Command::new(&program);
    cmd.args(&oc_args);
    // No briefing on stdin (it rode the positional); null stdin so the child never
    // blocks. Piped stdout (we parse the JSONL), inherited stderr (the human sees
    // opencode's own progress/errors / `--print-logs` if the user enabled it).
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    // Scrub elanus's provider credentials so opencode uses its OWN login rather than
    // inheriting elanus's DeepSeek/ANTHROPIC_*/OPENAI_* env. opencode does not read
    // any PROVIDER_CRED_VARS as its own auth (it stores creds in its auth.json), so
    // the scrub never removes opencode's login. The ELANUS_* vars set below are NOT
    // scrubbed.
    scrub_provider_creds(&mut cmd);
    scrub_launch_control_env(&mut cmd);
    cmd.env_remove("ELANUS_PACKAGE")
        .env_remove("ELANUS_BUS_TOKEN");
    // The session's own identity, carried to anything the opencode session spawns
    // (crucially `elanus code deliver`, which reads ELANUS_CODE_SESSION/AGENT).
    cmd.env(ENV_SESSION, session)
        .env(ENV_AGENT, agent)
        .env("ELANUS_ROOT", &root.dir);
    eprintln!("[code] launching opencode run --format json as session {session}{timeout_suffix}");

    let mut child = cmd
        .spawn()
        .with_context(|| format!("launching {program} (is it installed and on PATH?)"))?;

    // Capture the stream: persist the durable record on the first event carrying
    // opencode's native `sessionID` (so resume works after the launcher exits), and
    // harvest the legible result (final text + changed files) for routed completion.
    let summary = capture_opencode_stream(
        root,
        principal,
        bus_token,
        agent,
        session,
        &mut child,
        Some(workdir),
    );
    print_codex_worker_result(session, &summary);

    let status = child.wait().context("waiting for opencode run to finish")?;
    Ok((status, summary))
}

/// Read an opencode `run --format json` child's JSONL stdout line-by-line, mapping
/// each event to an obs record and publishing it as the session principal. Shared
/// by launch and resume. When `record_workdir` is `Some`, the FIRST event carrying
/// a native `sessionID` persists/refreshes the durable `code_sessions` record (the
/// launch path); resume already has a record and passes `None`. A malformed line
/// files generically (nothing dropped); a read error stops the loop but never
/// aborts. Returns the worker's verbatim final text (the last settled `text`) + the
/// file paths it reported writing — the legible result for the routed completion.
fn capture_opencode_stream(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    child: &mut std::process::Child,
    record_workdir: Option<&Path>,
) -> CaptureSummary {
    let mut summary = CaptureSummary::default();
    let mut recorded = false;
    let Some(out) = child.stdout.take() else {
        return summary;
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
                // A non-JSON line (opencode shouldn't emit one under --format json,
                // but be defensive): record it generically rather than drop it.
                let (leaf, body) = generic_event("opencode_nonjson_line", &Value::Null);
                publish_obs(
                    root,
                    principal,
                    bus_token,
                    &obs_topic(agent, session, &leaf),
                    body,
                );
                continue;
            }
        };
        // Harvest the legible result alongside publishing obs.
        opencode_collect_summary(&event, &mut summary);
        // The DURABLE session record (OC2): opencode's native `sessionID` is on
        // every event. Persist the record the moment we first see it, so the
        // session is resumable even after this launcher exits. Best-effort: a
        // record-write failure never breaks the live session.
        if let Some(workdir) = record_workdir {
            if !recorded {
                if let Some(sid) = event.get("sessionID").and_then(Value::as_str) {
                    if !sid.is_empty() {
                        let rec = codesession::SessionRecord {
                            elanus_session: session.to_string(),
                            native_session: sid.to_string(),
                            tool: "opencode".to_string(),
                            agent_noun: agent.to_string(),
                            workdir: workdir.display().to_string(),
                            // The room (if any) was set at launch via set_room;
                            // room:None preserves it (upsert COALESCE).
                            room: None,
                        };
                        if let Err(e) = codesession::upsert_record(root, &rec) {
                            eprintln!("[code] recording opencode session (continuing): {e:#}");
                        }
                        recorded = true;
                    }
                }
            }
        }
        for (leaf, body) in opencode_map_event(&event) {
            publish_obs(
                root,
                principal,
                bus_token,
                &obs_topic(agent, session, &leaf),
                body,
            );
        }
    }
    summary
}

/// Harvest the legible result from one opencode stream event into `summary`: the
/// text of each settled `text` event (so the LAST one wins — the verbatim final
/// answer, capped/marked) and the file path of each settled file-writing
/// `tool_use`. opencode's built-in `edit`/`write` tools declare their argument as
/// `path` (verified against the binary's tool Input structs + the server OpenAPI
/// `/doc` — the `state.input` in the JSON stream is the model's raw tool arguments,
/// which key the file as `path`, NOT `filePath`). Reads the SAME settled events
/// `opencode_map_event` files as obs; collecting here keeps that mapping untouched.
fn opencode_collect_summary(event: &Value, summary: &mut CaptureSummary) {
    match event.get("type").and_then(Value::as_str) {
        Some("text") => {
            if let Some(text) = event
                .get("part")
                .and_then(|p| p.get("text"))
                .and_then(Value::as_str)
            {
                // Last settled text wins (the worker's final word).
                summary.final_text = Some(clip(text, FINAL_TEXT_CAP));
            }
        }
        Some("tool_use") => {
            let part = event.get("part");
            let tool = part
                .and_then(|p| p.get("tool"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if opencode_is_file_writer(tool) {
                // The changed path is in the tool input under `path` (the field name
                // the built-in edit/write tool schemas declare; the JSON stream's
                // `state.input` is the model's raw tool arguments).
                if let Some(path) = part
                    .and_then(|p| p.get("state"))
                    .and_then(|s| s.get("input"))
                    .and_then(|i| i.get("path"))
                    .and_then(Value::as_str)
                {
                    summary.note_change(path);
                }
            }
        }
        _ => {}
    }
}

/// opencode's built-in tools that write a single file via a top-level `path`
/// argument (so a settled `tool_use` for one carries a `path` input the worker
/// changed on disk). Matched against the `tool` name in the stream's tool part.
/// `apply_patch` is deliberately excluded: it takes a multi-file patch blob (no
/// single `path`/`filePath`), so its changed paths live inside the patch text, not
/// a top-level input field.
fn opencode_is_file_writer(tool: &str) -> bool {
    matches!(tool, "edit" | "write")
}

/// Map one opencode `run --format json` stream event to obs/ topic leaves + bodies,
/// matching the exec.rs grammar (`tool/<name>/{call,result}`, `assistant/message`,
/// `session/idle`). Returns a `Vec` because a single settled `tool_use` projects
/// BOTH a `tool/<n>/call` (the input) and a `tool/<n>/result` (the output) — opencode
/// emits one combined settled tool event, but the obs grammar is call→result like
/// the other adapters. Anything unmodeled still lands via `generic_event` (nothing
/// dropped). Event `type` values pinned against opencode 1.17.9 (see the schema
/// comment above): `text`, `tool_use`, `reasoning`, `step_start`, `step_finish`,
/// `error`.
fn opencode_map_event(event: &Value) -> Vec<(String, Value)> {
    let ts = now_iso();
    let etype = event.get("type").and_then(Value::as_str).unwrap_or("");
    let part = event.get("part");
    match etype {
        // The assistant's settled message to the user.
        "text" => vec![(
            "assistant/message".into(),
            json!({
                "ts": ts,
                "text": clip_opt(part.and_then(|p| p.get("text")), 4000),
            }),
        )],
        // The model's settled reasoning trace.
        "reasoning" => vec![(
            "assistant/reasoning".into(),
            json!({
                "ts": ts,
                "text": clip_opt(part.and_then(|p| p.get("text")), 4000),
            }),
        )],
        // A settled tool use. opencode emits one combined event (input + output) on
        // completed/error; project it as call→result like the other adapters so a
        // tool reads consistently across harnesses.
        "tool_use" => {
            let tool = part
                .and_then(|p| p.get("tool"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let tool_seg = topic::encode_segment(tool);
            let state = part.and_then(|p| p.get("state"));
            let call_id = part
                .and_then(|p| p.get("callID"))
                .cloned()
                .unwrap_or(Value::Null);
            let status = state
                .and_then(|s| s.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let failed = status == "error";
            let call = (
                format!("tool/{tool_seg}/call"),
                json!({
                    "ts": ts,
                    "call_id": call_id,
                    "tool": tool,
                    "input": clip_value(state.and_then(|s| s.get("input")), 2000),
                }),
            );
            let result_body = if failed {
                json!({
                    "ts": ts,
                    "call_id": call_id,
                    "tool": tool,
                    "failed": true,
                    "error": clip_value(state.and_then(|s| s.get("error")), 4000),
                })
            } else {
                json!({
                    "ts": ts,
                    "call_id": call_id,
                    "tool": tool,
                    "failed": false,
                    "output": clip_value(state.and_then(|s| s.get("output")), 4000),
                })
            };
            let result = (format!("tool/{tool_seg}/result"), result_body);
            vec![call, result]
        }
        // Step boundaries: file step-finish (carries token usage / cost) as an idle
        // signal; skip the bare step-start (no useful payload).
        "step_start" => vec![],
        "step_finish" => vec![(
            "session/idle".into(),
            json!({
                "ts": ts,
                "event": "step_finish",
                "tokens": part.and_then(|p| p.get("tokens")).cloned().unwrap_or(Value::Null),
                "cost": part.and_then(|p| p.get("cost")).cloned().unwrap_or(Value::Null),
            }),
        )],
        // A session/stream error.
        "error" => vec![(
            "session/idle".into(),
            json!({
                "ts": ts,
                "event": "error",
                "error": clip_value(event.get("error"), 4000),
            }),
        )],
        // Anything else still lands, tagged by its event type, so nothing is
        // silently dropped.
        other => {
            let (leaf, mut body) = generic_event(other, event);
            if let Value::Object(m) = &mut body {
                m.insert("opencode_event".into(), json!(other));
            }
            vec![(leaf, body)]
        }
    }
}

/// The legible result of one capture pass — the worker's REAL output, harvested
/// from its own stream as we publish obs, so a routed completion can carry the
/// worker's verbatim answer + the files it touched (M4-A follow-on) instead of a
/// generated summary. `final_text` is the worker's actual last message (None when
/// it produced no text); `file_changes` are the on-disk paths the tool itself
/// reported writing (deduped, in first-seen order).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CaptureSummary {
    pub final_text: Option<String>,
    pub file_changes: Vec<String>,
}

impl CaptureSummary {
    /// Record a file path the tool reported writing, deduping (a session may edit
    /// the same file twice; the parent wants the set, in first-seen order).
    fn note_change(&mut self, path: impl Into<String>) {
        let path = path.into();
        if path.is_empty() || self.file_changes.iter().any(|p| p == &path) {
            return;
        }
        self.file_changes.push(path);
    }
}

/// Print the legible result of a blocking Codex launch to the caller's stdout.
/// The same summary has already been harvested while publishing obs; this is the
/// in-band surface a live parent can read without any bus authority. Keep the
/// format marked and plain so another tool can scrape it if needed.
fn print_codex_worker_result(session: &str, summary: &CaptureSummary) {
    println!("=== codex worker result (session {session}) ===");
    match summary.final_text.as_deref() {
        Some(text) if !text.trim().is_empty() => {
            println!("{text}");
        }
        _ => {
            println!("(no final text)");
        }
    }
    if summary.file_changes.is_empty() {
        println!("files changed: (none)");
    } else {
        println!("files changed: {}", summary.file_changes.join(", "));
    }
}

/// Print the legible result of a headless Claude worker launch to stdout. This
/// mirrors the Codex worker marker but keeps Claude's stdout verbatim because
/// `claude -p` already emits the headless final answer as plain text.
fn print_claude_worker_result(session: &str, text: &str) {
    println!("=== claude worker result (session {session}) ===");
    if text.trim().is_empty() {
        println!("(no final text)");
    } else {
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
    }
}

/// A generous cap for the worker's verbatim final text on the routed completion:
/// real bytes cut + marked (NOT a summary). Large enough to carry a substantive
/// answer; bounded so a runaway final message can't bloat a delivery payload.
const FINAL_TEXT_CAP: usize = 8000;

/// Read a codex child's `--json` stdout line-by-line, mapping each JSONL event to
/// an obs record and publishing it as the session principal. Shared by launch and
/// resume (the SAME obs grammar lands under the SAME elanus session both times).
/// When `record_workdir` is `Some`, a `thread.started` event also persists/refreshes
/// the durable `code_sessions` record (launch path, carrying the workdir to store);
/// resume already has a record, so it passes `None`. A malformed line files
/// generically (nothing dropped); a read error stops the loop but never aborts.
/// Returns the worker's verbatim final `agent_message` text (capped, marked when
/// cut) + the `file_change` paths it reported — the legible result for the routed
/// completion. The obs are still published exactly as before; this is in addition.
fn capture_codex_stream(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    child: &mut std::process::Child,
    record_workdir: Option<&Path>,
) -> CaptureSummary {
    let mut summary = CaptureSummary::default();
    let Some(out) = child.stdout.take() else {
        return summary;
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
                publish_obs(
                    root,
                    principal,
                    bus_token,
                    &obs_topic(agent, session, &leaf),
                    body,
                );
                continue;
            }
        };
        // Harvest the legible result alongside publishing obs: the LAST settled
        // `agent_message` is the worker's verbatim final answer; every settled
        // `file_change` reports the paths it wrote. (Codex settles both on
        // `item.completed`.)
        codex_collect_summary(&event, &mut summary);
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
                        // The room (if any) was set on the record at launch via
                        // set_room; room:None here preserves it (upsert COALESCE).
                        room: None,
                    };
                    if let Err(e) = codesession::upsert_record(root, &rec) {
                        eprintln!("[code] recording codex session (continuing): {e:#}");
                    }
                }
            }
        }
        if let Some((leaf, body)) = codex_map_event(&event) {
            publish_obs(
                root,
                principal,
                bus_token,
                &obs_topic(agent, session, &leaf),
                body,
            );
        }
    }
    summary
}

/// Harvest the legible result from one codex stream event into `summary`: the text
/// of each settled `agent_message` (so the LAST one wins — the worker's verbatim
/// final answer, capped/marked) and the paths of each settled `file_change`. Reads
/// the SAME settled items `codex_map_item` files as obs; collecting here keeps that
/// mapping untouched. Anything else is ignored.
fn codex_collect_summary(event: &Value, summary: &mut CaptureSummary) {
    if event.get("type").and_then(Value::as_str) != Some("item.completed") {
        return;
    }
    let Some(item) = event.get("item") else {
        return;
    };
    match item.get("type").and_then(Value::as_str) {
        Some("agent_message") => {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                // Last settled agent_message wins (the worker's final word).
                summary.final_text = Some(clip(text, FINAL_TEXT_CAP));
            }
        }
        Some("file_change") => {
            // `changes` is an array of { path, kind }; collect the path strings.
            if let Some(changes) = item.get("changes").and_then(Value::as_array) {
                for change in changes {
                    if let Some(path) = change.get("path").and_then(Value::as_str) {
                        summary.note_change(path);
                    }
                }
            }
        }
        _ => {}
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
// READ CAMERA scope (read-provenance handoff, M1): Codex's `exec --json` stream
// does NOT surface per-file reads — its items are command/mcp/agent_message, and
// Codex reads happen inside its `shell`/`apply_patch` commands (shell-buried,
// source B). So this adapter deliberately projects no `obs/fs` read event; Codex
// read provenance falls to M2 (the authoritative cage read camera, below the
// shell), which is platform-gated (macOS accepted-gap) and DEFERRED.
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
        // opencode resume = `opencode run --session <id> --format json --pure
        // --dangerously-skip-permissions "<msg>"` (confirmed flags against opencode
        // 1.17.9: `opencode run --session <id> "<msg>"` resumes a durable session;
        // --format json gives the same JSONL stream the launch path parses; --pure
        // drops external plugins; --dangerously-skip-permissions is the headless
        // auto-approve a driven resume needs). The workdir is applied by the caller
        // as the child cwd.
        "opencode" => (
            "opencode".to_string(),
            vec![
                "run".to_string(),
                "--session".to_string(),
                rec.native_session.clone(),
                "--format".to_string(),
                "json".to_string(),
                "--pure".to_string(),
                "--dangerously-skip-permissions".to_string(),
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

/// Wall-clock ceiling on a single resume's native model turn. A resume is one
/// turn (a real model round trip + any tool calls it makes); a few minutes is
/// generous for the headless `-p`/`exec` shapes while still bounding a wedged
/// run. The native call is wrapped in `timeout(1)` so a hung model never holds
/// a session worker (or a CLI invocation) open forever. Override per run with
/// `ELANUS_CODE_RESUME_TIMEOUT_S`.
const RESUME_TIMEOUT_SECS: u64 = 600;

fn resume_timeout_secs() -> u64 {
    std::env::var("ELANUS_CODE_RESUME_TIMEOUT_S")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&s: &u64| s > 0)
        .unwrap_or(RESUME_TIMEOUT_SECS)
}

/// Wall-clock ceiling on a detached spawned worker. This is deliberately much
/// larger than a driven resume timeout because a spawned delegation may do a real
/// chunk of work, but it must still eventually release the spawner wake path if a
/// native tool wedges. Override per run with `ELANUS_CODE_SPAWN_TIMEOUT_SECS`.
const SPAWN_TIMEOUT_SECS: u64 = 1800;

fn spawn_timeout_secs() -> u64 {
    std::env::var("ELANUS_CODE_SPAWN_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&s: &u64| s > 0)
        .unwrap_or(SPAWN_TIMEOUT_SECS)
}

/// Wrap a resume command in `timeout(1) -s TERM <secs> <program> <args…>` so a
/// hung native turn is killed rather than holding the caller open forever (the
/// handoff guardrail: wrap any codex/claude call in `timeout`). `timeout` is in
/// coreutils/BSD on every platform elanus targets; if it is somehow absent the
/// child simply fails to spawn and the resume errors cleanly (no hang). The
/// `-s TERM` lets the tool flush; `timeout` exits 124 on expiry, which the
/// caller reports as a failed (timed-out) resume.
fn timeout_wrap(program: &str, args: &[String], secs: u64) -> (String, Vec<String>) {
    let mut wrapped = vec![
        "-s".to_string(),
        "TERM".to_string(),
        secs.to_string(),
        program.to_string(),
    ];
    wrapped.extend_from_slice(args);
    ("timeout".to_string(), wrapped)
}

/// `elanus code resume <elanus_session> "<message>"` — the CLI entry. Runs the
/// resume in-process and PROPAGATES the tool's exit code via `process::exit` so a
/// script driving the launcher sees it. The daemon must NEVER use this path (a
/// worker tool's non-zero exit would kill the whole daemon); it calls
/// `resume_capture`, which returns the outcome instead of exiting.
pub fn resume(root: &Root, elanus_session: &str, message: &str) -> Result<()> {
    let outcome = resume_capture(root, elanus_session, message)?;
    if !outcome.success {
        std::process::exit(outcome.exit_code.unwrap_or(1));
    }
    Ok(())
}

/// The structured result of one driven/CLI resume — enough for the daemon to
/// thread a completion obs and settle the delivery event without ever exiting.
/// `final_text` + `file_changes` are the worker's LEGIBLE result (its verbatim last
/// message and the files it wrote), harvested from the capture stream so the routed
/// completion carries the worker's real answer (M4-A follow-on) — not a summary.
pub struct ResumeOutcome {
    pub success: bool,
    pub exit_code: Option<i32>,
    /// The worker's verbatim final message (capped/marked when huge); None when it
    /// produced no final text.
    pub final_text: Option<String>,
    /// The on-disk paths the worker reported writing this turn (deduped, possibly
    /// empty).
    pub file_changes: Vec<String>,
}

/// Continue a recorded coding session with a fresh, emit-only scoped token,
/// capturing the result under the same elanus session, and RETURN the outcome
/// (never `process::exit`). This is the in-process resume primitive the daemon
/// drives off a mailbox delivery (M2-B). Returns an error only for a missing
/// record or a credential/spawn failure; a non-zero tool exit is a successful
/// call with `success=false` (the daemon records it, the session lives on).
///
/// The native resume command is wrapped in `timeout` (handoff guardrail) and run
/// non-interactively (empty stdin, piped stdout we parse, inherited stderr). The
/// token is emit-only — minted here, retired at the end, reaped on crash — so a
/// driven resume gains the session NO read authority (M3's interactive-pull
/// remains deferred); the DAEMON, which already has authority, is the only reader
/// of the mailbox.
pub fn resume_capture(root: &Root, elanus_session: &str, message: &str) -> Result<ResumeOutcome> {
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
    // Resume re-mints a credential for the SAME session — it is not a new spawn,
    // so it is not charged against any spawner budget (the budget was consumed
    // at launch time, not at resume). Pass spawner=None, requested_budget=None.
    let token = codesession::mint(root, &principal, &rec.agent_noun, std::process::id() as i32,
                                  None, None)
        .with_context(|| format!("minting the resume credential for {principal}"))?;
    let bus_token = token.secret.clone();
    let agent = rec.agent_noun.clone();
    let session = rec.elanus_session.clone();
    let workdir = std::path::PathBuf::from(&rec.workdir);

    // M3 per-turn injection (the headless/driven path, both adapters). A driven
    // resume does NOT fire the launch-time UserPromptSubmit hook (a bare
    // `-p --resume` / `codex exec resume` doesn't reload the generated hooks), so
    // the per-turn context rides the RESUME PROMPT instead — prepended as an
    // out-of-band `[elanus]` block ahead of the delivered message. It carries the
    // session's inbox status + memory note (the same own-inbox-only scoped read,
    // built from this session's own noun/id), kept per-turn so it reflects the
    // current state every resume. The injection is prepended to the message the
    // model sees; it is NOT cached (a resume is a fresh turn), so it never busts
    // any prompt cache. None when there's nothing to inject (a quiet turn).
    let injected = build_resume_message(root, &agent, &session, message);

    let (program, cmd_args) = resume_command(&rec, &injected);
    // Bound the native turn (handoff guardrail): timeout(1) kills a hung model.
    let secs = resume_timeout_secs();
    let (program, cmd_args) = timeout_wrap(&program, &cmd_args, secs);

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

    // The capture summary (the worker's verbatim final text + the files it wrote),
    // harvested in the closure below and carried out to the ResumeOutcome.
    let mut summary = CaptureSummary::default();
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
        // Scrub elanus's provider credentials so the resumed tool uses its own
        // login rather than inheriting elanus's provider env (Task 2). The native
        // tool (claude/codex) is wrapped in `timeout`, but env_remove on the parent
        // Command is inherited through the `timeout` exec, so the tool still never
        // sees them.
        scrub_provider_creds(&mut cmd);
        eprintln!(
            "[code] resuming {} session {session} ({}) [timeout {secs}s]",
            rec.tool, rec.native_session
        );
        let mut child = cmd.spawn().with_context(|| {
            format!("launching {program} resume (is it installed and on PATH?)")
        })?;

        match rec.tool.as_str() {
            // Each adapter's resume emits a JSONL stream on stdout. Codex's `exec
            // resume --json` and opencode's `run --session --format json` are
            // identical to their launch streams (record_workdir=None — we already
            // have a record). Claude's `-p --output-format stream-json` is a
            // DIFFERENT JSONL grammar; map it via the CC stream mapper.
            "codex" => {
                // record_workdir = None: the record already exists (we read it).
                summary = capture_codex_stream(
                    root, &principal, &bus_token, &agent, &session, &mut child, None,
                );
            }
            "opencode" => {
                // opencode `run --session <id> --format json` emits the SAME JSONL
                // stream the launch path parses. record_workdir = None (the record
                // already exists). The resume targets the recorded native session.
                summary = capture_opencode_stream(
                    root, &principal, &bus_token, &agent, &session, &mut child, None,
                );
            }
            _ => {
                summary = capture_claude_stream(
                    root, &principal, &bus_token, &agent, &session, &mut child,
                );
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
    Ok(ResumeOutcome {
        success: status.success(),
        exit_code: status.code(),
        final_text: summary.final_text,
        file_changes: summary.file_changes,
    })
}

/// Read a Claude Code `-p --output-format stream-json` child's stdout line-by-line,
/// mapping each JSONL message to an obs record under the resumed elanus session.
/// Claude's print stream is a different grammar from codex's: top-level objects
/// with a `type` of `system` (init), `assistant`/`user` (message turns carrying a
/// nested `message` with `content` blocks: `text`, `tool_use`, `tool_result`), and
/// `result` (the final settle, carrying `result` text + `session_id` + usage). We
/// map the load-bearing ones onto the existing obs grammar so a resumed turn reads
/// like a launched one; anything unmodeled lands generically (nothing dropped).
/// Returns the worker's verbatim final answer (the `result` frame's `result` text,
/// capped/marked) + the file paths from each file-writing `tool_use` block — the
/// legible result for the routed completion, harvested as we publish obs.
fn capture_claude_stream(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    child: &mut std::process::Child,
) -> CaptureSummary {
    let mut summary = CaptureSummary::default();
    let Some(out) = child.stdout.take() else {
        return summary;
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
        // Harvest the legible result alongside publishing obs: the final `result`
        // frame's text is the worker's verbatim answer; every file-writing
        // `tool_use` block reports a path it wrote.
        claude_collect_summary(&event, &mut summary);
        if let Some((leaf, body)) = claude_stream_map(&event) {
            publish_obs(
                root,
                principal,
                bus_token,
                &obs_topic(agent, session, &leaf),
                body,
            );
        }
    }
    summary
}

/// The Claude Code tools that write a file (so a `tool_use` for one carries a
/// `file_path` the worker changed on disk). Matched case-sensitively against the
/// tool name in the print stream's `tool_use` block.
fn claude_is_file_writer(tool: &str) -> bool {
    matches!(tool, "Write" | "Edit" | "MultiEdit" | "NotebookEdit")
}

/// Harvest the legible result from one Claude print-stream event into `summary`:
/// the `result` frame's `result` text (the worker's verbatim final answer,
/// capped/marked) and the `file_path` of every file-writing `tool_use` block.
/// Reads the same frames `claude_stream_map`/`claude_stream_message` file as obs;
/// collecting here leaves that mapping untouched.
fn claude_collect_summary(event: &Value, summary: &mut CaptureSummary) {
    match event.get("type").and_then(Value::as_str) {
        Some("result") => {
            if let Some(text) = event.get("result").and_then(Value::as_str) {
                summary.final_text = Some(clip(text, FINAL_TEXT_CAP));
            }
        }
        Some("assistant") | Some("user") => {
            let Some(content) = event
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array)
            else {
                return;
            };
            for block in content {
                if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                let Some(tool) = block.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if !claude_is_file_writer(tool) {
                    continue;
                }
                if let Some(path) = block
                    .get("input")
                    .and_then(|i| i.get("file_path"))
                    .and_then(Value::as_str)
                {
                    summary.note_change(path);
                }
            }
        }
        _ => {}
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
                let tool = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
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
/// session principal. Always exits 0: a hook that fails must never break or alter
/// the coding session. It prints to stdout only for the M3 per-turn injection —
/// on `UserPromptSubmit`/`SessionStart` it emits a `hookSpecificOutput`
/// `additionalContext` object (the system-reminder layer, Appendix A) carrying the
/// session's inbox status + memory note; every other event prints nothing.
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
                // The room (if any) was set on the record at launch via set_room;
                // room:None here preserves it (upsert COALESCE).
                room: None,
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

    // READ CAMERA — M1 (read-provenance handoff). For Claude Code only, ALSO
    // project read-shaped tool calls (Read/Grep/Glob) into the WRITE camera's
    // spatial, path-keyed noun: an `obs/fs/<encoded-canonical-path>` event with
    // `op: "read"`, `via: "tool"`, carrying the causing `tool_use_id` — so "what
    // did this agent read, and when" is the same `obs/fs/<subtree>/#` subscription
    // the write side already affords. These ride the SAME default-none recorder
    // rule as write deltas (`obs/fs/#` → Sink::None in src/recorder.rs): opt-in per
    // subtree, never recorded-by-default. Advisory/honest-agent tier only — see
    // `claude_read_fs_events`. Codex/opencode are NOT projected here (Codex reads
    // are shell-buried → M2; opencode is out of M1's Claude-Code-only scope).
    if event == "PreToolUse" && matches!(Tool::from_agent_noun(&agent), Some(Tool::ClaudeCode)) {
        for (topic_name, fs_body) in claude_read_fs_events(&payload, &session) {
            publish_obs(root, &principal, &token, &topic_name, fs_body);
        }
    }

    // M3 per-turn injection (Claude Code): on UserPromptSubmit (and SessionStart),
    // return the session's inbox status + memory note as `additionalContext` — the
    // SYSTEM-REMINDER layer (Appendix A: stdout of these hooks lands as an
    // out-of-band system note AFTER the cached prefix, not in the user message).
    // This is the per-turn counterpart to the one-time launch briefing: each turn
    // the agent sees how many messages are waiting and any note a planner left.
    // The inbox read here is the SAME own-inbox-only scoped query — built from the
    // session's own env-derived `agent`/`session`, never a hook-supplied id — so a
    // hook can never surface another session's inbox. D5 adds a focused NUDGE on
    // UserPromptSubmit when the user's prompt mentions dispatch/delegation terms:
    // this teaches the front door for live coding workers on the exact turn where
    // the agent is likely to need it, while preserving a fully quiet turn when
    // there is no inbox/note/peer-claim context and no relevant prompt.
    if matches!(event, "UserPromptSubmit" | "SessionStart") {
        let ctx = turn_injection(root, &agent, &session);
        let hint = (event == "UserPromptSubmit" && user_prompt_mentions_dispatch(&payload))
            .then(|| DISPATCH_HINT.to_string());
        let additional_context = match (ctx, hint) {
            (Some(ctx), Some(hint)) => Some(format!("{ctx}\n{hint}")),
            (Some(ctx), None) => Some(ctx),
            (None, Some(hint)) => Some(hint),
            (None, None) => None,
        };
        if let Some(additional_context) = additional_context {
            // The documented JSON form (Appendix A): hookSpecificOutput with the
            // hook event name + additionalContext. Printed on stdout (exit 0) so
            // Claude Code folds it into the system-reminder layer.
            let out = json!({
                "hookSpecificOutput": {
                    "hookEventName": event,
                    "additionalContext": additional_context,
                }
            });
            println!("{out}");
        }
    }
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
                common(
                    json!({ "tool": tool, "input": clip_value(payload.get("tool_input"), 4000) }),
                ),
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

/// READ CAMERA — M1 (read-provenance handoff). Project Claude Code's read-shaped
/// tool calls into the SAME spatial, path-keyed shape as the WRITE camera
/// (`src/exec.rs::emit_fs_delta` → `obs/fs/<encoded-canonical-path>`), so "what did
/// this agent read, and when" is the same `obs/fs/<subtree>/#` subscription the
/// write side already affords — not a tool-noun scan, not the agent's fuzzy memory.
///
/// Given a Claude Code `PreToolUse` payload, return zero or more
/// `(topic, body)` read events. The body mirrors the write delta's shape with
/// `op: "read"` and adds `via: "tool"` so a consumer can tell the read flavor from
/// the write flavor on the shared noun, and carries the causing `tool_use_id` as
/// `cause` — attribution is structural (the hook IS the bracket), exactly like the
/// write camera carrying `tool_call_id`.
///
/// HONESTY / SCOPE — load-bearing, also stamped on every event as `scope`:
/// - This is the ADVISORY, honest-agent tier (handoff wonky-bit #2): it records an
///   honest agent's Read/Grep/Glob, and a `Bash`+`cat` walks straight around it.
///   It is NOT the safety boundary. The authoritative version is M2 (cage camera).
/// - CLAUDE CODE ONLY. Codex's `exec --json` stream does not surface per-file reads
///   (its items are command/mcp/agent_message — reads are shell-buried), so Codex
///   reads fall to M2; `codex_map_event` deliberately emits no read events.
///   opencode's stream does carry its `read`/`grep`/`glob` tool calls, but M1 is
///   scoped to Claude Code per the handoff; opencode reads also fall to M2.
/// - Covers SOURCE (A) explicit tool reads only. SOURCE (B) shell-buried reads
///   (`cat`, `<`, build inputs inside a `Bash` tool call) and SOURCE (C) context
///   auto-loads (CLAUDE.md, MCP resources, injected reminders) are OUT OF SCOPE
///   here — this is stamped on each event (`covers: "A"`, `omits: ["B","C"]`) so a
///   consumer never reads an empty/partial result as "no reads happened."
/// - Read SHAPE differs by tool: `Read` opens a CONCRETE file (`file_path`); `Grep`
///   and `Glob` read a search ROOT (a directory/pattern, optional `path`, defaulting
///   to cwd) — NOT a single opened file. We label the locus (`locus: "file"` vs
///   `"search-root"`) so a consumer isn't misled into treating a search root as a
///   read of one file.
///
/// M2 (the authoritative cage read camera, sits below the shell — catches source B)
/// and M3 (the status surface / fast-fail subscribe) are DEFERRED: M2 is
/// platform-gated (macOS has no free authoritative read-open notification — needs
/// the Endpoint-Security entitlement + a signed system extension; the unprivileged
/// seccomp-unotify path is Linux-only) AND gated on coding-agents.md's tool-sandbox
/// bypass (coding agents are not in elanus's cage today). See the read-provenance
/// handoff. M1 is the only tier that delivers on macOS now.
fn claude_read_fs_events(payload: &Value, session: &str) -> Vec<(String, Value)> {
    let tool = tool_name(payload);
    // Locus differs: Read opens a concrete file; Grep/Glob read a search root.
    let (input_key, locus) = match tool.as_str() {
        "Read" => ("file_path", "file"),
        "Grep" | "Glob" => ("path", "search-root"),
        // Not a read-shaped tool — no fs read event (Write/Edit/Bash etc. are the
        // write camera's or out-of-scope source B).
        _ => return Vec::new(),
    };
    let input = payload.get("tool_input");
    // Read's path is required; Grep/Glob's `path` is optional (a search rooted at
    // cwd when absent). Fall back to the hook's cwd so a search still has a locus —
    // emitted honestly via the `locus` label, never as a concrete file read.
    let raw_path = input
        .and_then(|i| i.get(input_key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let cwd = payload.get("cwd").and_then(Value::as_str);
    let path = match (raw_path, cwd) {
        (Some(p), _) => p.to_string(),
        // No path arg (Grep/Glob search rooted at cwd): use the cwd as the locus.
        (None, Some(c)) => c.to_string(),
        // No path and no cwd — nothing honest to key on; skip rather than fabricate.
        (None, None) => return Vec::new(),
    };
    // Mirror the write camera: key on the encoded canonical absolute path. A
    // relative path (Grep/Glob may pass one) is resolved against the hook's cwd;
    // canonicalize best-effort, falling back to the lexical join so a path that no
    // longer exists (or a glob pattern) still keys spatially rather than dropping.
    let p = PathBuf::from(&path);
    let abs = if p.is_absolute() {
        p
    } else if let Some(c) = cwd {
        Path::new(c).join(&p)
    } else {
        p
    };
    let canon = std::fs::canonicalize(&abs).unwrap_or(abs);
    let tool_use_id = payload
        .get("tool_use_id")
        .cloned()
        .unwrap_or(Value::Null);
    let cc_session = payload.get("session_id").cloned().unwrap_or(Value::Null);
    let topic_name = format!("obs/fs/{}", topic::encode_path(&canon));
    let body = json!({
        "op": "read",
        "via": "tool",
        "tool": tool,
        // SESSION ATTRIBUTION — the milestone's acceptance ("the human pulls the
        // read stream FOR THE SESSION"). The spatial obs/fs/<path> topic carries no
        // session level by design, so stamp the elanus `session` (and the native CC
        // `cc_session`) onto the body — matching what the write camera's envelope
        // carries on the shared obs/fs noun (emit_fs_delta stamps session_id via
        // trace::Ids), so a consumer of obs/fs/<subtree>/# can session-scope READ
        // events just as it can WRITE events. `cause` alone (the opaque tool_use_id)
        // is not a session key and does not join the obs/agent/.../tool/Read stream.
        "session": session,
        "cc_session": cc_session,
        // The read locus: a concrete opened file (Read) vs a search root (Grep/Glob).
        "locus": locus,
        // The original argument as the agent wrote it (relative or a glob pattern),
        // kept alongside the canonical topic so a search pattern isn't lost.
        "arg": path,
        // Structural attribution: the causing tool_use, exactly as the write camera
        // carries `tool_call_id`.
        "cause": tool_use_id,
        // HONEST SCOPE stamped on the event (see fn doc): advisory, not the safety
        // boundary; covers source (A) only; (B) shell-buried + (C) context
        // auto-loads are NOT witnessed here — never read an empty result as
        // "no reads happened."
        "tier": "advisory",
        "covers": "A",
        "omits": ["B", "C"],
        "ts": now_iso(),
    });
    vec![(topic_name, body)]
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
        assert!(matches!(Tool::parse("opencode"), Ok(Tool::OpenCode)));
        assert!(matches!(Tool::parse("oc"), Ok(Tool::OpenCode)));
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
        // opencode is also a StreamJson adapter (no hooks, no settings).
        assert!(matches!(Tool::OpenCode.capture(), Capture::StreamJson));
        assert_eq!(Tool::OpenCode.agent_noun(), "opencode");
        assert_eq!(Tool::OpenCode.binary(), "opencode");
        assert!(matches!(
            Tool::from_agent_noun("opencode"),
            Some(Tool::OpenCode)
        ));
        let dummy_root = Root {
            dir: PathBuf::from("/tmp/fake-root"),
        };
        assert!(
            Tool::OpenCode
                .settings(Path::new("/usr/local/bin/elanus"), &dummy_root)
                .is_none()
        );
        assert!(
            Tool::Codex
                .settings(Path::new("/usr/local/bin/elanus"), &dummy_root)
                .is_none()
        );
        assert!(
            Tool::ClaudeCode
                .settings(Path::new("/usr/local/bin/elanus"), &dummy_root)
                .is_some()
        );
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
        assert!(
            body["response"]
                .as_str()
                .unwrap()
                .contains("permission denied")
        );
    }

    // ── READ CAMERA (M1, read-provenance handoff) ───────────────────────────

    #[test]
    fn read_tool_projects_an_obs_fs_read_event() {
        // A Read tool call projects an obs/fs/<encoded-canonical-path> event in the
        // WRITE camera's spatial noun, with op:"read", via:"tool", and the causing
        // tool_use id as `cause` — so write and read share `obs/fs/<subtree>/#`.
        let payload = json!({
            "session_id": "cc-123",
            "cwd": "/tmp/proj",
            "hook_event_name": "PreToolUse",
            "tool_name": "Read",
            "tool_use_id": "toolu_ABC",
            "tool_input": { "file_path": "/tmp/proj/notes.md" },
        });
        let events = claude_read_fs_events(&payload, "sess-42");
        assert_eq!(events.len(), 1, "one read locus for a Read");
        let (topic_name, body) = &events[0];
        // Same spatial topic shape the write camera emits (encode_path, leading
        // slash dropped) — a consumer's obs/fs/tmp/proj/# matches both flavors.
        assert_eq!(
            *topic_name,
            format!("obs/fs/{}", topic::encode_path(Path::new("/tmp/proj/notes.md")))
        );
        assert!(topic::valid_name(topic_name));
        assert!(topic::matches("obs/fs/tmp/proj/#", topic_name));
        assert_eq!(body["op"], "read");
        assert_eq!(body["via"], "tool");
        assert_eq!(body["tool"], "Read");
        assert_eq!(body["locus"], "file");
        // Structural attribution: the causing tool_use, like the write camera's
        // tool_call_id.
        assert_eq!(body["cause"], "toolu_ABC");
        // Honest scope stamped on the event so an empty read stream is never
        // misread as "no reads happened".
        assert_eq!(body["tier"], "advisory");
        assert_eq!(body["covers"], "A");
        assert_eq!(body["omits"], json!(["B", "C"]));
        // SESSION ATTRIBUTION — the milestone's acceptance ("pull the read stream
        // FOR THE SESSION"). The spatial obs/fs/<path> topic has no session level,
        // so the body must carry the elanus `session` (matching the write camera's
        // envelope on the shared noun) and the native CC `cc_session`.
        assert_eq!(body["session"], "sess-42");
        assert_eq!(body["cc_session"], "cc-123");
    }

    #[test]
    fn grep_and_glob_project_honestly_as_search_roots() {
        // Grep/Glob read a search ROOT (a directory/pattern), NOT a single opened
        // file. We emit the path arg as the locus and label it search-root so a
        // consumer isn't misled into treating it as a concrete file read.
        let grep = json!({
            "cwd": "/tmp/proj",
            "tool_name": "Grep",
            "tool_use_id": "toolu_G",
            "tool_input": { "pattern": "TODO", "path": "/tmp/proj/src" },
        });
        let g = claude_read_fs_events(&grep, "sess-g");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].1["locus"], "search-root");
        assert_eq!(g[0].1["op"], "read");
        assert_eq!(g[0].1["tool"], "Grep");
        assert!(topic::matches("obs/fs/tmp/proj/#", &g[0].0));

        // Glob with no `path` arg searches from cwd — emit honestly keyed on cwd
        // (the search locus), never fabricating a concrete file.
        let glob = json!({
            "cwd": "/tmp/proj",
            "tool_name": "Glob",
            "tool_use_id": "toolu_X",
            "tool_input": { "pattern": "**/*.rs" },
        });
        let gl = claude_read_fs_events(&glob, "sess-gl");
        assert_eq!(gl.len(), 1);
        assert_eq!(gl[0].1["locus"], "search-root");
        assert_eq!(
            gl[0].0,
            format!("obs/fs/{}", topic::encode_path(Path::new("/tmp/proj")))
        );
    }

    #[test]
    fn non_read_tools_project_no_fs_read_event() {
        // Bash/Write/Edit are NOT read-shaped: Bash's reads are shell-buried
        // (source B, M2's job); Write/Edit are the write camera. No read event.
        for tool in ["Bash", "Write", "Edit", "WebFetch"] {
            let payload = json!({
                "cwd": "/tmp/proj",
                "tool_name": tool,
                "tool_use_id": "toolu_N",
                "tool_input": { "command": "cat /etc/passwd", "file_path": "/x" },
            });
            assert!(
                claude_read_fs_events(&payload, "sess-n").is_empty(),
                "{tool} must not project a read event"
            );
        }
    }

    // The read flavor's default-none recorder property (it inherits obs/fs/#) is
    // asserted in recorder.rs::tests::read_camera_flavor_inherits_default_none,
    // where the recorder internals are in scope.

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
        assert!(
            codex_map_event(&json!({
                "type": "item.started",
                "item": { "id": "item_1", "type": "agent_message", "text": "" }
            }))
            .is_none()
        );
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
        assert!(
            call_body["command"]
                .as_str()
                .unwrap()
                .contains("echo hello")
        );

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
        assert!(
            codex_map_event(&json!({
                "type": "item.started",
                "item": { "id": "item_3", "type": "file_change", "status": "in_progress" }
            }))
            .is_none()
        );
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
        let token =
            codesession::mint(&root, principal, "claude-code", std::process::id() as i32,
                              None, None).unwrap();
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
            room: None,
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
    fn resume_command_opencode_targets_the_recorded_session() {
        // opencode resume = `opencode run --session <id> --format json --pure
        // --dangerously-skip-permissions "<msg>"` (confirmed flags against opencode
        // 1.17.9). The recorded native session id is the resume target; the workdir
        // is applied by the caller as the child cwd.
        let rec = codesession::SessionRecord {
            elanus_session: "code-cccc3333".to_string(),
            native_session: "ses_112e4b951ffeKBRlwfWTyi0a7A".to_string(),
            tool: "opencode".to_string(),
            agent_noun: "opencode".to_string(),
            workdir: "/tmp/proj".to_string(),
            room: None,
        };
        let (prog, args) = resume_command(&rec, "carry on");
        assert_eq!(prog, "opencode");
        assert_eq!(
            args,
            vec![
                "run",
                "--session",
                "ses_112e4b951ffeKBRlwfWTyi0a7A",
                "--format",
                "json",
                "--pure",
                "--dangerously-skip-permissions",
                "carry on",
            ]
        );
        // The session id is positional right after `--session` — the resume targets
        // THE recorded session.
        assert_eq!(args[2], rec.native_session);
    }

    #[test]
    fn opencode_map_event_projects_the_obs_grammar() {
        // A settled assistant text → assistant/message.
        let evs = opencode_map_event(&json!({
            "type": "text",
            "sessionID": "ses_a",
            "part": { "type": "text", "text": "pong" }
        }));
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].0, "assistant/message");
        assert_eq!(evs[0].1["text"], "pong");

        // A settled tool_use → BOTH tool/<n>/call (input) and tool/<n>/result
        // (output), like the other adapters.
        let evs = opencode_map_event(&json!({
            "type": "tool_use",
            "sessionID": "ses_a",
            "part": {
                "type": "tool",
                "tool": "bash",
                "callID": "call_1",
                "state": {
                    "status": "completed",
                    "input": { "command": "ls" },
                    "output": "file.txt"
                }
            }
        }));
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].0, "tool/bash/call");
        assert_eq!(evs[0].1["input"], "{\"command\":\"ls\"}");
        assert_eq!(evs[1].0, "tool/bash/result");
        assert_eq!(evs[1].1["failed"], false);
        assert_eq!(evs[1].1["output"], "file.txt");

        // An errored tool_use marks failed + carries the error.
        let evs = opencode_map_event(&json!({
            "type": "tool_use",
            "sessionID": "ses_a",
            "part": {
                "type": "tool",
                "tool": "edit",
                "callID": "call_2",
                "state": { "status": "error", "input": {}, "error": "nope" }
            }
        }));
        assert_eq!(evs[1].0, "tool/edit/result");
        assert_eq!(evs[1].1["failed"], true);
        assert_eq!(evs[1].1["error"], "nope");
    }

    #[test]
    fn opencode_unknown_event_type_lands_generically_nothing_dropped() {
        // An event type this binary doesn't model still lands, tagged by type.
        let evs = opencode_map_event(&json!({ "type": "future.kind", "sessionID": "ses_a" }));
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].0, "event/future.kind");
        assert_eq!(evs[0].1["opencode_event"], "future.kind");
    }

    #[test]
    fn opencode_collect_summary_harvests_final_text_and_changed_files() {
        let mut summary = CaptureSummary::default();
        opencode_collect_summary(
            &json!({ "type": "text", "part": { "text": "first" } }),
            &mut summary,
        );
        opencode_collect_summary(
            &json!({ "type": "text", "part": { "text": "final" } }),
            &mut summary,
        );
        // Last settled text wins.
        assert_eq!(summary.final_text.as_deref(), Some("final"));

        // A file-writing tool reports its path. opencode's built-in write/edit
        // tools key the file under `path` (verified against the binary's tool Input
        // structs + the server OpenAPI `/doc`), NOT `filePath`.
        opencode_collect_summary(
            &json!({
                "type": "tool_use",
                "part": {
                    "tool": "write",
                    "state": { "status": "completed", "input": { "path": "/tmp/x.rs", "content": "x" } }
                }
            }),
            &mut summary,
        );
        assert_eq!(summary.file_changes, vec!["/tmp/x.rs"]);
        // The `edit` tool also keys its target under `path`.
        opencode_collect_summary(
            &json!({
                "type": "tool_use",
                "part": {
                    "tool": "edit",
                    "state": { "status": "completed",
                               "input": { "path": "/tmp/y.rs", "oldString": "a", "newString": "b" } }
                }
            }),
            &mut summary,
        );
        assert_eq!(summary.file_changes, vec!["/tmp/x.rs", "/tmp/y.rs"]);

        // A non-writing tool does not add a change.
        opencode_collect_summary(
            &json!({
                "type": "tool_use",
                "part": { "tool": "bash", "state": { "status": "completed", "input": {} } }
            }),
            &mut summary,
        );
        assert_eq!(summary.file_changes, vec!["/tmp/x.rs", "/tmp/y.rs"]);
    }

    #[test]
    fn opencode_message_folds_the_brief_into_the_positional() {
        // With a brief, it rides ahead of the task (opencode has no out-of-band
        // system-prompt flag).
        let m = opencode_message_with_brief(Some("be careful"), "do the thing");
        assert!(m.contains("be careful"));
        assert!(m.ends_with("do the thing"));
        // Without a brief, the task is verbatim.
        assert_eq!(
            opencode_message_with_brief(None, "do the thing"),
            "do the thing"
        );
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
            room: None,
        };
        let (prog, args) = resume_command(&rec, "continue please");
        assert_eq!(prog, "claude");
        assert!(args.contains(&"-p".to_string()));
        let resume_pos = args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(args[resume_pos + 1], "cc-sess-9f");
        assert!(
            args.windows(2)
                .any(|w| w == ["--output-format", "stream-json"])
        );
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

    // ── Capture summary: the worker's verbatim result (M4-A follow-on) ─────────

    #[test]
    fn codex_capture_summary_takes_last_message_and_all_file_paths() {
        let mut s = CaptureSummary::default();
        // Two agent_messages: the LAST one is the worker's final word.
        codex_collect_summary(
            &json!({ "type": "item.completed",
                     "item": { "id": "i1", "type": "agent_message", "text": "first" } }),
            &mut s,
        );
        codex_collect_summary(
            &json!({ "type": "item.completed",
                     "item": { "id": "i2", "type": "agent_message", "text": "ALPHA" } }),
            &mut s,
        );
        // A file_change item reports paths via `changes: [{ path, kind }]`.
        codex_collect_summary(
            &json!({ "type": "item.completed",
                     "item": { "id": "i3", "type": "file_change",
                               "changes": [{ "path": "src/foo.rs", "kind": "update" },
                                           { "path": "src/bar.rs", "kind": "add" }] } }),
            &mut s,
        );
        // A second change to the same file dedupes (set, first-seen order).
        codex_collect_summary(
            &json!({ "type": "item.completed",
                     "item": { "id": "i4", "type": "file_change",
                               "changes": [{ "path": "src/foo.rs", "kind": "update" }] } }),
            &mut s,
        );
        // An item.started (not completed) is ignored — the text settles on completed.
        codex_collect_summary(
            &json!({ "type": "item.started",
                     "item": { "id": "i5", "type": "agent_message", "text": "partial" } }),
            &mut s,
        );
        assert_eq!(
            s.final_text.as_deref(),
            Some("ALPHA"),
            "the LAST agent_message is verbatim"
        );
        assert_eq!(
            s.file_changes,
            vec!["src/foo.rs", "src/bar.rs"],
            "all changed paths, deduped"
        );
    }

    #[test]
    fn claude_capture_summary_takes_result_text_and_file_writer_paths() {
        let mut s = CaptureSummary::default();
        // A file-writing tool_use reports its path; a non-writer (Bash) does not.
        claude_collect_summary(
            &json!({ "type": "assistant", "session_id": "cc",
                     "message": { "content": [
                         { "type": "tool_use", "name": "Write", "input": { "file_path": "/w/a.rs" } } ] } }),
            &mut s,
        );
        claude_collect_summary(
            &json!({ "type": "assistant", "session_id": "cc",
                     "message": { "content": [
                         { "type": "tool_use", "name": "Edit", "input": { "file_path": "/w/b.rs" } } ] } }),
            &mut s,
        );
        claude_collect_summary(
            &json!({ "type": "assistant", "session_id": "cc",
                     "message": { "content": [
                         { "type": "tool_use", "name": "Bash", "input": { "command": "ls" } } ] } }),
            &mut s,
        );
        // The final `result` frame carries the verbatim answer text.
        claude_collect_summary(
            &json!({ "type": "result", "subtype": "success", "session_id": "cc",
                     "result": "ALPHA", "is_error": false }),
            &mut s,
        );
        assert_eq!(
            s.final_text.as_deref(),
            Some("ALPHA"),
            "the result frame text is verbatim"
        );
        assert_eq!(
            s.file_changes,
            vec!["/w/a.rs", "/w/b.rs"],
            "only file-writer tool paths"
        );
        assert!(claude_is_file_writer("MultiEdit") && claude_is_file_writer("NotebookEdit"));
        assert!(!claude_is_file_writer("Read") && !claude_is_file_writer("Grep"));
    }

    #[test]
    fn capture_summary_final_text_is_truncated_not_summarized() {
        // A huge final message is CLIPPED to real bytes + marked — never summarized.
        let big = "X".repeat(FINAL_TEXT_CAP + 500);
        let mut s = CaptureSummary::default();
        codex_collect_summary(
            &json!({ "type": "item.completed",
                     "item": { "id": "i", "type": "agent_message", "text": big } }),
            &mut s,
        );
        let ft = s.final_text.unwrap();
        assert!(
            ft.starts_with(&"X".repeat(FINAL_TEXT_CAP)),
            "the head is the worker's real bytes"
        );
        assert!(
            ft.contains("clipped"),
            "truncation is marked, not summarized"
        );
    }

    // ── Inbound delivery recognition (M2-B) ──────────────────────────────────

    fn delivery_tmp_root() -> Root {
        let dir = std::env::temp_dir().join(format!(
            "elanus-delivery-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    #[test]
    fn recognize_matches_a_recorded_session_mailbox() {
        let root = delivery_tmp_root();
        codesession::upsert_record(
            &root,
            &codesession::SessionRecord {
                elanus_session: "code-abcd1234".into(),
                native_session: "thread-x".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: "/tmp/proj".into(),
                room: None,
            },
        )
        .unwrap();
        // The documented address resolves to (session, noun).
        let got = recognize_delivery(&root, "in/agent/codex/code-abcd1234");
        assert_eq!(got, Some(("code-abcd1234".into(), "codex".into())));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn recognize_rejects_non_session_addresses() {
        let root = delivery_tmp_root();
        codesession::upsert_record(
            &root,
            &codesession::SessionRecord {
                elanus_session: "code-abcd1234".into(),
                native_session: "thread-x".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: "/tmp/proj".into(),
                room: None,
            },
        )
        .unwrap();
        // An ordinary agent's mailbox (non-code conv) is not a coding session.
        assert!(recognize_delivery(&root, "in/agent/kestrel/c123").is_none());
        // A never-recorded code-* conv is ignored cleanly (no panic, no resume).
        assert!(recognize_delivery(&root, "in/agent/codex/code-00000000").is_none());
        // The wrong noun for the record (typo / cross-drive attempt) is rejected.
        assert!(recognize_delivery(&root, "in/agent/claude-code/code-abcd1234").is_none());
        // Wrong verb/category, too few/many levels, an obs topic — all None.
        assert!(recognize_delivery(&root, "obs/agent/codex/code-abcd1234").is_none());
        assert!(recognize_delivery(&root, "in/human/owner").is_none());
        assert!(recognize_delivery(&root, "in/agent/codex").is_none());
        assert!(recognize_delivery(&root, "in/agent/codex/code-abcd1234/extra").is_none());
        // A path-traversal-shaped conv is not a valid session principal.
        assert!(recognize_delivery(&root, "in/agent/codex/code-..%2Fowner").is_none());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn recognize_decodes_an_encoded_conv_segment() {
        // The launcher encodes the conv with topic::encode_segment; recognition
        // must decode it back to the record key. A session id with a reserved
        // char round-trips through encode → topic → decode.
        let root = delivery_tmp_root();
        let sess = "code-a+b"; // '+' would split a level if not encoded
        codesession::upsert_record(
            &root,
            &codesession::SessionRecord {
                elanus_session: sess.into(),
                native_session: "t".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: "/tmp".into(),
                room: None,
            },
        )
        .unwrap();
        let topic = format!("in/agent/codex/{}", topic::encode_segment(sess));
        assert!(topic::valid_name(&topic)); // the '+' is encoded, no wildcard
        assert_eq!(
            recognize_delivery(&root, &topic),
            Some((sess.into(), "codex".into()))
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn delivery_message_accepts_prompt_text_or_bare_string() {
        // The documented field.
        assert_eq!(
            delivery_message(&json!({ "prompt": "do the thing" })).as_deref(),
            Some("do the thing")
        );
        // The convenience alias.
        assert_eq!(
            delivery_message(&json!({ "text": "also fine" })).as_deref(),
            Some("also fine")
        );
        // prompt wins over text when both are present.
        assert_eq!(
            delivery_message(&json!({ "prompt": "p", "text": "t" })).as_deref(),
            Some("p")
        );
        // A bare JSON string is taken verbatim.
        assert_eq!(
            delivery_message(&json!("just text")).as_deref(),
            Some("just text")
        );
        // Nothing drivable → None (the daemon skips rather than resume on nothing).
        assert!(delivery_message(&json!({ "other": "x" })).is_none());
        assert!(delivery_message(&json!({ "prompt": "" })).is_none());
        assert!(delivery_message(&Value::Null).is_none());
    }

    // ── Requester capture + idempotency key (M4-A) ───────────────────────────

    #[test]
    fn idempotency_key_prefers_explicit_then_event_id() {
        // An explicit key in the payload wins.
        assert_eq!(
            idempotency_key(
                &json!({ "prompt": "x", "idempotency_key": "planner-step-3" }),
                42
            ),
            "planner-step-3"
        );
        // Otherwise the stable inbound event id (survives the at-least-once replay,
        // which re-pends the SAME row with the SAME id).
        assert_eq!(idempotency_key(&json!({ "prompt": "x" }), 42), "event:42");
        // A blank explicit key falls back too.
        assert_eq!(
            idempotency_key(&json!({ "idempotency_key": "  " }), 7),
            "event:7"
        );
    }

    #[test]
    fn requester_from_explicit_reply_to_topic() {
        let root = delivery_tmp_root();
        // A full in/agent/<noun>/<conv> topic whose conv is a RECORDED coding
        // session resolves to that session's own mailbox (re-derived, not verbatim)
        // — the legitimate planner-reply form M4-A uses.
        codesession::upsert_record(
            &root,
            &codesession::SessionRecord {
                elanus_session: "code-planner1".into(),
                native_session: "thr".into(),
                tool: "claude".into(),
                agent_noun: "claude-code".into(),
                workdir: "/tmp".into(),
                room: None,
            },
        )
        .unwrap();
        let req = delivery_requester(
            &root,
            &json!({ "prompt": "go", "reply_to": "in/agent/claude-code/code-planner1" }),
            Some("owner"),
            Some("corr-1"),
        )
        .unwrap();
        assert_eq!(req.reply_to, "in/agent/claude-code/code-planner1");
        // A bare agent name resolves to that agent's mailbox, the conversation from
        // the correlation.
        let req = delivery_requester(
            &root,
            &json!({ "prompt": "go", "reply_to": "kestrel" }),
            Some("owner"),
            Some("corr-1"),
        )
        .unwrap();
        assert_eq!(req.reply_to, "in/agent/kestrel/corr-1");
        // A full in/agent/<noun>/<conv> topic for a NATIVE agent re-derives the
        // agent mailbox with the named conversation (not verbatim, but identical
        // shape for a well-formed agent address).
        let req = delivery_requester(
            &root,
            &json!({ "prompt": "go", "reply_to": "in/agent/kestrel/room-7" }),
            Some("owner"),
            None,
        )
        .unwrap();
        assert_eq!(req.reply_to, "in/agent/kestrel/room-7");
        // A wildcard reply_to is rejected (not routable).
        assert!(
            delivery_requester(
                &root,
                &json!({ "prompt": "go", "reply_to": "in/agent/+/code-x" }),
                None,
                None,
            )
            .is_none()
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    /// M4-A confused-deputy (security): an explicit `reply_to` must resolve to a
    /// RECOGNIZED actor's mailbox; it can NEVER coax a kernel-authored completion
    /// onto the human inbox or an arbitrary topic. These are the exact abuse probes
    /// the adversarial verify turned up — each must yield None (no route), so no
    /// kernel message can ever land on those topics via reply_to.
    #[test]
    fn explicit_reply_to_cannot_target_human_inbox_or_arbitrary_topic() {
        let root = delivery_tmp_root();
        // The headline exploit: route a kernel-authored completion to the owner's
        // human inbox. REJECTED.
        assert!(
            delivery_requester(
                &root,
                &json!({ "prompt": "x", "reply_to": "in/human/owner" }),
                Some("owner"),
                None,
            )
            .is_none(),
            "reply_to in/human/owner must not route"
        );
        // An arbitrary non-mailbox topic. REJECTED.
        assert!(
            delivery_requester(
                &root,
                &json!({ "prompt": "x", "reply_to": "in/totally/arbitrary/x" }),
                Some("owner"),
                None,
            )
            .is_none(),
            "reply_to to an arbitrary in/ topic must not route"
        );
        // Other-plane topics a verbatim route would have published a kernel message
        // onto: work/, signal/, obs/. All REJECTED.
        for bad in [
            "signal/cancel/all",
            "obs/agent/codex/code-victim/session/start",
            "work/agent/exec",
            "in/group/secret-room",
            "in/human/owner/extra",
            "in/agent/codex",               // too few levels (not a mailbox)
            "in/agent/codex/code-x/thread", // too many levels
            "in/agent/codex/code-ghost",    // a code-* conv with NO record → not an actor
            "in/agent/+/code-x",            // a wildcard
            "in/agent/codex/#",             // a wildcard
        ] {
            assert!(
                delivery_requester(
                    &root,
                    &json!({ "prompt": "x", "reply_to": bad }),
                    Some("owner"),
                    None
                )
                .is_none(),
                "reply_to {bad:?} must not route a kernel message"
            );
        }
        // And a bare name that is path-unsafe / reserved cannot be coaxed into a
        // non-agent topic level either.
        assert!(
            delivery_requester(
                &root,
                &json!({ "prompt": "x", "reply_to": "../../owner" }),
                Some("owner"),
                None,
            )
            .is_none()
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn requester_from_sender_coding_session_routes_to_its_own_mailbox() {
        let root = delivery_tmp_root();
        // A planner that is a recorded coding session: its sender resolves to its
        // OWN session mailbox, so the completion resumes it (the loop closing).
        codesession::upsert_record(
            &root,
            &codesession::SessionRecord {
                elanus_session: "code-planner1".into(),
                native_session: "thr".into(),
                tool: "claude".into(),
                agent_noun: "claude-code".into(),
                workdir: "/tmp".into(),
                room: None,
            },
        )
        .unwrap();
        let req = delivery_requester(
            &root,
            &json!({ "prompt": "do work" }),
            Some("code-planner1"),
            Some("corr-9"),
        )
        .unwrap();
        assert_eq!(req.reply_to, "in/agent/claude-code/code-planner1");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn requester_none_for_owner_kernel_or_unrecorded_session() {
        let root = delivery_tmp_root();
        // The human owner / kernel originating a plain delivery is NOT a planner
        // waiting on a completion — no routing (the M2-B behavior, unchanged).
        assert!(
            delivery_requester(&root, &json!({ "prompt": "x" }), Some("owner"), None).is_none()
        );
        assert!(
            delivery_requester(&root, &json!({ "prompt": "x" }), Some("kernel"), None).is_none()
        );
        // No sender and no reply_to → nothing to route to.
        assert!(delivery_requester(&root, &json!({ "prompt": "x" }), None, None).is_none());
        // A code-* sender with no durable record can't be addressed → None (not a
        // panic, not a bogus route).
        assert!(
            delivery_requester(&root, &json!({ "prompt": "x" }), Some("code-ghost00"), None)
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn requester_from_native_agent_sender_uses_correlation_conv() {
        let root = delivery_tmp_root();
        // A native (non-code) agent sender routes to its own mailbox, the
        // correlation as the conversation locator.
        let req = delivery_requester(
            &root,
            &json!({ "prompt": "x" }),
            Some("kestrel"),
            Some("c42"),
        )
        .unwrap();
        assert_eq!(req.reply_to, "in/agent/kestrel/c42");
        // No correlation → a stable default conversation.
        let req =
            delivery_requester(&root, &json!({ "prompt": "x" }), Some("kestrel"), None).unwrap();
        assert_eq!(req.reply_to, "in/agent/kestrel/main");
        let _ = std::fs::remove_dir_all(&root.dir);
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

    // ── The deliver tool (M4-B) ──────────────────────────────────────────────

    fn record_session(root: &Root, sess: &str, noun: &str) {
        codesession::upsert_record(
            root,
            &codesession::SessionRecord {
                elanus_session: sess.into(),
                native_session: "thr".into(),
                tool: if noun == "codex" { "codex" } else { "claude" }.into(),
                agent_noun: noun.into(),
                workdir: "/tmp".into(),
                room: None,
            },
        )
        .unwrap();
    }

    /// Read back the most recent event a `record_delivery` emitted (id, type,
    /// sender, payload, correlation, state).
    fn read_event(root: &Root, id: i64) -> (String, String, String, String, String) {
        let conn = crate::db::open(root).unwrap();
        conn.query_row(
            "SELECT type, sender, COALESCE(payload,''), COALESCE(correlation_id,''), state
             FROM events WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap()
    }

    #[test]
    fn deliver_builds_the_worker_mailbox_delivery_recording_the_requester() {
        let root = delivery_tmp_root();
        // A recorded planner (the requester) and a recorded codex worker.
        record_session(&root, "code-planner1", "claude-code");
        record_session(&root, "code-worker1", "codex");

        let id = record_delivery(
            &root,
            "code-planner1",
            "code-worker1",
            "  build the thing  ",
        )
        .unwrap();
        let (etype, sender, payload, corr, state) = read_event(&root, id);

        // The delivery is addressed to the worker's session mailbox — exactly the
        // address the daemon's recognize_delivery resumes.
        assert_eq!(etype, "in/agent/codex/code-worker1");
        assert!(recognize_delivery(&root, &etype).is_some());
        // It is recorded with the planner as the sender (honest provenance — M4-A's
        // requester capture reads this).
        assert_eq!(sender, "code-planner1");
        // The event is pending — the daemon drives it next tick. Not announced as a
        // session bus publish (the emit-only token was never used).
        assert_eq!(state, "pending");
        // The message (trimmed) is the prompt; the reply_to is the planner.
        let pv: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(pv["prompt"], "build the thing");
        assert_eq!(pv["reply_to"], "code-planner1");
        assert!(!corr.is_empty());

        // The captured requester from this delivery resolves to the planner's own
        // mailbox — so M4-A routes the completion back and resumes it (the loop).
        let req = delivery_requester(&root, &pv, Some(&sender), Some(&corr)).unwrap();
        assert_eq!(req.reply_to, "in/agent/claude-code/code-planner1");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn deliver_to_unknown_worker_fails_cleanly() {
        let root = delivery_tmp_root();
        record_session(&root, "code-planner1", "claude-code");
        let err = record_delivery(&root, "code-planner1", "code-ghost000", "do it").unwrap_err();
        assert!(format!("{err:#}").contains("no coding session"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn deliver_rejects_self_delivery_and_empty_message() {
        let root = delivery_tmp_root();
        record_session(&root, "code-self0001", "codex");
        // A session cannot dispatch to itself (would self-resume into a loop).
        let err = record_delivery(&root, "code-self0001", "code-self0001", "go").unwrap_err();
        assert!(format!("{err:#}").contains("own session"));
        // An empty message is rejected (nothing to act on).
        record_session(&root, "code-worker9", "codex");
        let err = record_delivery(&root, "code-self0001", "code-worker9", "   ").unwrap_err();
        assert!(format!("{err:#}").contains("must not be empty"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn deliver_omits_reply_to_when_requester_unrecorded_but_still_records_sender() {
        // A freshly-launched planner whose native id isn't observed yet has no
        // record. The delivery still emits (sender carries the provenance), just
        // without an explicit reply_to — M4-A re-derives from the sender once the
        // record exists, or routes nothing if it never does (no crash).
        let root = delivery_tmp_root();
        record_session(&root, "code-worker1", "codex");
        let id = record_delivery(&root, "code-planner-unrec", "code-worker1", "x").unwrap();
        let (_etype, sender, payload, _corr, _state) = read_event(&root, id);
        assert_eq!(sender, "code-planner-unrec");
        let pv: Value = serde_json::from_str(&payload).unwrap();
        assert!(pv.get("reply_to").is_none(), "no reply_to without a record");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn spawn_completion_delivery_uses_safe_mailbox_and_worker_sender() {
        let root = delivery_tmp_root();
        record_session(&root, "code-planner1", "claude-code");
        let summary = CaptureSummary {
            final_text: Some("finished the task".into()),
            file_changes: vec!["src/lib.rs".into(), "src/main.rs".into()],
        };

        let id = emit_completion_delivery(
            &root,
            "code-worker1",
            "code-planner1",
            Some("code-spawn-corr"),
            None,
            &summary,
            None,
        )
        .unwrap();
        let (etype, sender, payload, corr, state) = read_event(&root, id);

        assert_eq!(etype, "in/agent/claude-code/code-planner1");
        assert!(recognize_delivery(&root, &etype).is_some());
        assert_eq!(sender, "code-worker1");
        assert_eq!(corr, "code-spawn-corr");
        assert_eq!(state, "pending");
        let pv: Value = serde_json::from_str(&payload).unwrap();
        let prompt = pv["prompt"].as_str().unwrap();
        assert!(prompt.contains("Worker code-worker1 finished"));
        assert!(prompt.contains("finished the task"));
        assert!(prompt.contains("src/lib.rs"));
        assert!(prompt.contains("src/main.rs"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn completion_delivery_clips_large_file_change_lists() {
        let summary = CaptureSummary {
            final_text: Some("done".into()),
            file_changes: (0..55).map(|i| format!("src/file{i}.rs")).collect(),
        };
        let prompt = completion_delivery_prompt("code-worker1", None, &summary, None);

        assert!(prompt.contains("src/file0.rs"));
        assert!(prompt.contains("src/file49.rs"));
        assert!(!prompt.contains("src/file50.rs"));
        assert!(prompt.contains("… and 5 more"));
    }

    #[cfg(unix)]
    #[test]
    fn completion_delivery_names_timeout_exit_124() {
        use std::os::unix::process::ExitStatusExt as _;

        let status = std::process::ExitStatus::from_raw(124 << 8);
        let prompt = completion_delivery_prompt(
            "code-worker1",
            Some(&status),
            &CaptureSummary::default(),
            None,
        );

        assert!(prompt.contains("Status: timed out after "));
    }

    #[test]
    fn forced_session_token_file_blocks_forced_id_reuse() {
        let root = delivery_tmp_root();
        let principal = "code-live0001";
        codesession::mint(&root, principal, "codex", std::process::id() as i32, None, None).unwrap();

        assert!(forced_session_token_exists(&root, principal));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn timeout_wrap_uses_coreutils_timeout_shape() {
        let (program, args) = timeout_wrap("codex", &["exec".into(), "do it".into()], 1800);

        assert_eq!(program, "timeout");
        assert_eq!(
            args,
            vec![
                "-s".to_string(),
                "TERM".to_string(),
                "1800".to_string(),
                "codex".to_string(),
                "exec".to_string(),
                "do it".to_string(),
            ]
        );
    }

    // ── The launch-envelope briefing (M4-B) ──────────────────────────────────

    #[test]
    fn briefing_covers_the_envelope_essentials() {
        let b = briefing("code-abcd1234");
        // It names the session, the deliver command, the end-your-turn rule, and
        // the behave-normally-toward-the-human note.
        assert!(b.contains("code-abcd1234"));
        assert!(b.contains("elanus code deliver"));
        assert!(b.to_lowercase().contains("end your turn"));
        assert!(b.to_lowercase().contains("do not")); // do not poll/sleep/wait
        assert!(b.to_lowercase().contains("human"));
        // Short — a launch briefing, not a manual.
        assert!(
            b.len() < 1200,
            "briefing should be concise, was {} chars",
            b.len()
        );
    }

    #[test]
    fn brief_flag_is_on_by_default_and_strippable() {
        // No flag → briefing on, args unchanged.
        let (on, args) = take_brief_flag(&["exec".into(), "do x".into()]);
        assert!(on);
        assert_eq!(args, vec!["exec".to_string(), "do x".to_string()]);
        // --no-brief → off, and the flag is stripped so the tool never sees it.
        let (on, args) = take_brief_flag(&["--no-brief".into(), "do x".into()]);
        assert!(!on);
        assert_eq!(args, vec!["do x".to_string()]);
    }

    #[test]
    fn codex_briefing_block_wraps_the_brief_for_stdin() {
        // Codex gets the briefing on stdin (folded in as a `<stdin>` block) rather
        // than via arg injection, which would be fragile against flag values like
        // `-m <model>`. The block carries the full briefing body.
        let block = codex_briefing_block("BRIEF-BODY");
        assert!(block.contains("elanus operating envelope"));
        assert!(block.contains("BRIEF-BODY"));
    }

    #[test]
    fn elanus_skill_is_session_scratch_scoped() {
        let dir = std::env::temp_dir().join(format!(
            "elanus-skill-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let skill_root = write_elanus_skill(&dir).unwrap();
        let skill_path = skill_root
            .join(".claude")
            .join("skills")
            .join("elanus")
            .join("SKILL.md");
        let skill = std::fs::read_to_string(&skill_path).unwrap();
        assert!(skill.contains("name: elanus"));
        assert!(skill.contains("elanus code help"));
        assert!(skill.contains("elanus code claude --worker"));
        assert_eq!(skill_root, dir.join("skillroot"));
        assert!(!skill_root.join("settings.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── M3: per-turn injection + the inbox read ──────────────────────────────

    fn m3_tmp_root() -> Root {
        let dir = std::env::temp_dir().join(format!(
            "elanus-m3-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    fn m3_record(root: &Root, sess: &str, noun: &str) {
        codesession::upsert_record(
            root,
            &codesession::SessionRecord {
                elanus_session: sess.into(),
                native_session: "t".into(),
                tool: if noun == "codex" { "codex" } else { "claude" }.into(),
                agent_noun: noun.into(),
                workdir: "/tmp".into(),
                room: None,
            },
        )
        .unwrap();
    }

    fn m3_deliver(root: &Root, noun: &str, sess: &str, from: &str, msg: &str) -> i64 {
        let conn = crate::db::open(root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let topic = format!(
            "in/agent/{}/{}",
            topic::encode_segment(noun),
            topic::encode_segment(sess),
        );
        crate::events::emit(
            root,
            &conn,
            crate::events::EmitOpts {
                payload: Some(json!({ "prompt": msg })),
                sender: Some(from.to_string()),
                ..crate::events::EmitOpts::new(&topic)
            },
        )
        .unwrap()
    }

    #[test]
    fn turn_injection_reflects_inbox_and_note_and_changes_with_state() {
        let root = m3_tmp_root();
        m3_record(&root, "code-inj00001", "codex");

        // A quiet turn: nothing to inject.
        assert!(turn_injection(&root, "codex", "code-inj00001").is_none());

        // Deliver one message → the injection reports it (system-note style).
        m3_deliver(&root, "codex", "code-inj00001", "owner", "fix the parser");
        let one = turn_injection(&root, "codex", "code-inj00001").unwrap();
        assert!(one.starts_with("[elanus]"));
        assert!(one.contains("1 new message"));
        assert!(one.contains("elanus code inbox")); // tells the agent how to read
        assert!(one.contains("fix the parser")); // a brief preview

        // Deliver a second → the injection CHANGES (count reflects the new inbox).
        m3_deliver(
            &root,
            "codex",
            "code-inj00001",
            "code-planner",
            "and the lexer",
        );
        let two = turn_injection(&root, "codex", "code-inj00001").unwrap();
        assert!(two.contains("2 new message"));
        assert_ne!(
            one, two,
            "the injected text must change when the inbox changes"
        );

        // A memory note also surfaces, and changes when edited.
        codesession::set_note(&root, "code-inj00001", "the lexer lives in src/lex.rs").unwrap();
        let with_note = turn_injection(&root, "codex", "code-inj00001").unwrap();
        assert!(with_note.contains("[elanus note]"));
        assert!(with_note.contains("src/lex.rs"));
        codesession::set_note(&root, "code-inj00001", "actually src/lexer/mod.rs").unwrap();
        let edited = turn_injection(&root, "codex", "code-inj00001").unwrap();
        assert!(edited.contains("src/lexer/mod.rs"));
        assert!(!edited.contains("src/lex.rs"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn turn_injection_shows_only_unseen_inbox() {
        let root = m3_tmp_root();
        m3_record(&root, "code-uns00001", "codex");
        let id1 = m3_deliver(&root, "codex", "code-uns00001", "owner", "one");
        m3_deliver(&root, "codex", "code-uns00001", "owner", "two");
        // Two unseen.
        assert!(
            turn_injection(&root, "codex", "code-uns00001")
                .unwrap()
                .contains("2 new message")
        );
        // Pulling marks the first seen → the next turn reflects only the unseen.
        codesession::mark_inbox_seen(&root, "code-uns00001", &[id1]).unwrap();
        assert!(
            turn_injection(&root, "codex", "code-uns00001")
                .unwrap()
                .contains("1 new message")
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn build_resume_message_prepends_injection_only_when_present() {
        let root = m3_tmp_root();
        m3_record(&root, "code-bld00001", "codex");
        // No inbox / no note → the message is unchanged (a plain resume stays plain).
        let plain = build_resume_message(&root, "codex", "code-bld00001", "do the work");
        assert_eq!(plain, "do the work");
        // With a note, the `[elanus]` block is prepended and the delivered message
        // is kept under its own marker.
        codesession::set_note(&root, "code-bld00001", "remember X").unwrap();
        let injected = build_resume_message(&root, "codex", "code-bld00001", "do the work");
        assert!(injected.starts_with("[elanus]"));
        assert!(injected.contains("remember X"));
        assert!(injected.contains("do the work"));
        assert!(injected.contains("message you were resumed with"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn note_cmd_requires_a_recorded_session() {
        let root = m3_tmp_root();
        // No record → clean error (a note would otherwise sit unread).
        let err = note_cmd(&root, "code-nope0000", "hi").unwrap_err();
        assert!(format!("{err:#}").contains("no coding session"));
        // With a record, it round-trips through codesession.
        m3_record(&root, "code-ok000001", "codex");
        note_cmd(&root, "code-ok000001", "do the thing").unwrap();
        assert_eq!(
            codesession::get_note(&root, "code-ok000001")
                .unwrap()
                .as_deref(),
            Some("do the thing")
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── M5: room flag + peer-claim surfacing through the M3 injection ─────────

    #[test]
    fn take_room_flag_extracts_and_strips_room() {
        // --room <id> is parsed out and stripped from the args the tool sees.
        let (room, rest) =
            take_room_flag(&["--room".into(), "team-1".into(), "fix the bug".into()]);
        assert_eq!(room.as_deref(), Some("team-1"));
        assert_eq!(rest, vec!["fix the bug".to_string()]);
        // No --room → None, args untouched.
        let (room, rest) = take_room_flag(&["fix the bug".into()]);
        assert!(room.is_none());
        assert_eq!(rest, vec!["fix the bug".to_string()]);
        // A bare trailing --room (no value) is dropped, no room.
        let (room, rest) = take_room_flag(&["do it".into(), "--room".into()]);
        assert!(room.is_none());
        assert_eq!(rest, vec!["do it".to_string()]);
    }

    /// Record a session WITH a room, and join it, so turn_injection can derive the
    /// room from the record and read peer claims. Workdir `/tmp` (SA2 then treats
    /// two `/tmp` members as workdir-siblings — use `m5_member_wd` to separate).
    fn m5_member(root: &Root, room: &str, sess: &str) {
        m5_member_wd(root, room, sess, "/tmp");
    }

    /// Like `m5_member` but with an explicit workdir, so a test can keep two
    /// sessions out of each other's SA2 workdir-sibling roster.
    fn m5_member_wd(root: &Root, room: &str, sess: &str, workdir: &str) {
        codesession::upsert_record(
            root,
            &codesession::SessionRecord {
                elanus_session: sess.into(),
                native_session: "t".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: workdir.into(),
                room: Some(room.into()),
            },
        )
        .unwrap();
        codesession::join_room(root, room, sess, "codex", std::process::id() as i32).unwrap();
    }

    #[test]
    fn turn_injection_surfaces_peer_claims_excluding_own() {
        // The M5 payoff: B's per-turn injection shows A's claim ("code-A is editing
        // src/foo.rs") and does NOT list B's own claim. The room is derived from B's
        // OWN record — never an argument.
        let root = m3_tmp_root();
        m5_member(&root, "room-1", "code-aaaa0001"); // A
        m5_member(&root, "room-1", "code-bbbb0002"); // B
        codesession::add_claim(&root, "room-1", "code-aaaa0001", "src/foo.rs").unwrap();
        codesession::add_claim(&root, "room-1", "code-bbbb0002", "src/own.rs").unwrap();

        // B's injection: shows A's claim, excludes B's own.
        let b_inj = turn_injection(&root, "codex", "code-bbbb0002").unwrap();
        assert!(
            b_inj.contains("code-aaaa0001 is editing src/foo.rs"),
            "B must see A's claim: {b_inj}"
        );
        assert!(
            !b_inj.contains("src/own.rs"),
            "B must NOT see its own claim: {b_inj}"
        );
        assert!(
            b_inj.contains("advisory"),
            "the claim is presented as advisory: {b_inj}"
        );

        // A's injection: shows B's claim, excludes A's own.
        let a_inj = turn_injection(&root, "codex", "code-aaaa0001").unwrap();
        assert!(a_inj.contains("code-bbbb0002 is editing src/own.rs"));
        assert!(!a_inj.contains("src/foo.rs"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn turn_injection_room_isolation_no_cross_room_claims() {
        // A session in R1 never sees an R2 claim in its injection.
        let root = m3_tmp_root();
        // Distinct workdirs so SA2 does not make them workdir-siblings — this test
        // isolates the CLAIM-room boundary, not the workdir roster.
        m5_member_wd(&root, "R1", "code-r1aa0001", "/tmp");
        m5_member_wd(&root, "R2", "code-r2aa0001", "/var/tmp");
        codesession::add_claim(&root, "R2", "code-r2aa0001", "secret/r2.rs").unwrap();
        // The R1 session has no peers (its only roommate is itself), so a quiet
        // turn injects nothing about claims and never leaks R2's claim.
        let r1 = turn_injection(&root, "codex", "code-r1aa0001");
        assert!(r1.is_none() || !r1.unwrap().contains("secret/r2.rs"));
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn build_resume_message_carries_peer_claims_to_a_driven_turn() {
        // The headless/driven path (codex + driven CC resume): the per-turn claim
        // surfacing rides the resume prompt (build_resume_message), so a driven B
        // turn receives A's advisory claim out of band, ahead of the delivered
        // message.
        let root = m3_tmp_root();
        m5_member(&root, "room-1", "code-aaaa0001");
        m5_member(&root, "room-1", "code-bbbb0002");
        codesession::add_claim(&root, "room-1", "code-aaaa0001", "src/foo.rs").unwrap();
        let msg = build_resume_message(&root, "codex", "code-bbbb0002", "go do the task");
        assert!(
            msg.contains("code-aaaa0001 is editing src/foo.rs"),
            "the resume turn must carry the peer claim: {msg}"
        );
        assert!(
            msg.contains("go do the task"),
            "the delivered message is still present"
        );
        // The claim block precedes the delivered message (out of band).
        let claim_at = msg.find("is editing src/foo.rs").unwrap();
        let msg_at = msg.find("go do the task").unwrap();
        assert!(claim_at < msg_at);
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn solo_session_has_no_peers_and_no_claim_injection() {
        // A session launched without --room (room:None) has no peers; turn_injection
        // surfaces no claims (and is None on an otherwise-quiet turn).
        let root = m3_tmp_root();
        m3_record(&root, "code-solo0001", "codex"); // room: None, workdir /tmp
        // Even if some other session (in a room) holds a claim, a roomless session
        // in a DIFFERENT workdir sees nothing (a distinct dir so SA2 does not make
        // them workdir-siblings — this test is about claim-room isolation).
        m5_member_wd(&root, "other-room", "code-elsewhere", "/var/tmp");
        codesession::add_claim(&root, "other-room", "code-elsewhere", "x.rs").unwrap();
        assert!(turn_injection(&root, "codex", "code-solo0001").is_none());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── SA1: the workdir is the room (ambient claims, no flag) ────────────────

    #[test]
    fn sa1_resolve_room_derives_from_canonical_workdir() {
        // No explicit room → a stable, workdir-derived id with the `wd-` marker.
        let dir = std::env::temp_dir();
        let r1 = resolve_room(None, &dir);
        let r2 = resolve_room(None, &dir);
        assert!(r1.starts_with("wd-"), "default room is workdir-derived: {r1}");
        assert_eq!(r1, r2, "the same workdir must derive the SAME room (stable)");
        // A different dir derives a different room.
        let other = resolve_room(None, Path::new("/"));
        assert_ne!(r1, other, "distinct workdirs derive distinct rooms");
    }

    #[test]
    fn sa1_explicit_room_overrides_workdir_default() {
        let dir = std::env::temp_dir();
        let explicit = resolve_room(Some("team-1"), &dir);
        assert_eq!(explicit, "team-1", "an explicit --room wins over the workdir");
        // A blank/whitespace explicit value falls back to the workdir default.
        let blank = resolve_room(Some("   "), &dir);
        assert!(blank.starts_with("wd-"), "a blank --room falls back to workdir: {blank}");
    }

    #[test]
    fn sa1_two_flagless_sessions_in_same_dir_share_a_room_and_see_claims() {
        // The SA1 acceptance: two sessions recorded with NO room but the SAME
        // workdir derive the SAME room, so one's claim is the other's peer claim —
        // with zero flags. (We use a real, canonicalizable shared dir.)
        let root = m3_tmp_root();
        let shared = root.dir.join("checkout");
        std::fs::create_dir_all(&shared).unwrap();
        let wd = shared.display().to_string();
        // Both sessions: room: None (no --room), same workdir.
        for sess in ["code-amb00001", "code-amb00002"] {
            codesession::upsert_record(
                &root,
                &codesession::SessionRecord {
                    elanus_session: sess.into(),
                    native_session: "t".into(),
                    tool: "codex".into(),
                    agent_noun: "codex".into(),
                    workdir: wd.clone(),
                    room: None,
                },
            )
            .unwrap();
        }
        // Session 1 claims a path via the derived room (session_room_identity is
        // env-driven; here we derive the room directly and add the claim).
        let room = resolve_room(None, &shared);
        codesession::add_claim(&root, &room, "code-amb00001", "src/foo.rs").unwrap();
        // Session 2's injection surfaces it as a peer claim — no flags anywhere.
        let inj = turn_injection(&root, "codex", "code-amb00002").unwrap();
        assert!(
            inj.contains("code-amb00001 is editing src/foo.rs"),
            "a flagless same-dir sibling's claim must surface: {inj}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── SA2: live siblings in the per-turn injection ──────────────────────────

    #[test]
    fn sa2_injection_names_a_live_sibling_in_the_same_workdir() {
        // With a live sibling in the same workdir, the block prepends a one-line
        // siblings roster naming the other session and (from its claim) what it is
        // touching.
        let root = m3_tmp_root();
        let shared = root.dir.join("repo");
        std::fs::create_dir_all(&shared).unwrap();
        let wd = shared.display().to_string();
        // A is a live sibling of viewer B (both fresh, this process owns both pids).
        m5_member_wd(&root, "room-x", "code-livea001", &wd);
        m5_member_wd(&root, "room-x", "code-liveb002", &wd);
        codesession::add_claim(&root, "room-x", "code-livea001", "ui/App.tsx").unwrap();
        let b = turn_injection(&root, "codex", "code-liveb002").unwrap();
        assert!(
            b.contains("[elanus siblings]"),
            "the siblings roster line must appear: {b}"
        );
        assert!(b.contains("code-livea001"), "names the live sibling: {b}");
        assert!(
            b.contains("last editing ui/App.tsx"),
            "surfaces what the sibling is touching (from its claim): {b}"
        );
        assert!(
            b.contains("git worktree"),
            "SA4: a same-tree sibling triggers the worktree nudge: {b}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn sa2_solo_session_block_is_unchanged_no_sibling_line() {
        // A genuinely solo session (unique workdir, no sibling) gets NO siblings
        // line — the block is unchanged from M3/M5 (and None on a quiet turn).
        let root = m3_tmp_root();
        let shared = root.dir.join("alone");
        std::fs::create_dir_all(&shared).unwrap();
        let wd = shared.display().to_string();
        m5_member_wd(&root, "room-solo", "code-alone001", &wd);
        // Quiet turn, no siblings → nothing to say.
        assert!(turn_injection(&root, "codex", "code-alone001").is_none());
        // Give it an inbox message: the block appears but carries NO siblings line.
        m3_deliver(&root, "codex", "code-alone001", "owner", "hi");
        let inj = turn_injection(&root, "codex", "code-alone001").unwrap();
        assert!(inj.starts_with("[elanus]"), "solo block is unchanged: {inj}");
        assert!(
            !inj.contains("[elanus siblings]"),
            "a solo session must have no siblings line: {inj}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn sa2_dead_sibling_is_excluded_from_the_roster() {
        // A sibling whose owner pid is dead (pid 1 is not us; use an impossible pid)
        // must NOT appear in the roster — stale-session hygiene.
        let root = m3_tmp_root();
        let shared = root.dir.join("repo2");
        std::fs::create_dir_all(&shared).unwrap();
        let wd = shared.display().to_string();
        // Viewer, live.
        m5_member_wd(&root, "room-d", "code-viewerd1", &wd);
        // A "dead" sibling: record + a membership row with a pid that cannot be
        // alive. join_room takes a pid; pass one that pid_alive rejects.
        codesession::upsert_record(
            &root,
            &codesession::SessionRecord {
                elanus_session: "code-deadsib1".into(),
                native_session: "t".into(),
                tool: "codex".into(),
                agent_noun: "codex".into(),
                workdir: wd.clone(),
                room: Some("room-d".into()),
            },
        )
        .unwrap();
        // A pid that is not alive (i32::MAX is never a live process here).
        codesession::join_room(&root, "room-d", "code-deadsib1", "codex", i32::MAX).unwrap();
        let live = codesession::live_siblings(&root, "code-viewerd1", &wd);
        assert!(
            !live.iter().any(|s| s.session == "code-deadsib1"),
            "a dead-pid sibling must be excluded: {live:?}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn scrub_removes_provider_creds_but_keeps_elanus_vars() {
        // The denylist constant scrubs exactly the provider-credential vars and
        // nothing else; the ELANUS_* session/bus/root vars are not in it.
        for v in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_MODEL",
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "OPENAI_API_BASE",
            "OPENAI_MODEL",
        ] {
            assert!(PROVIDER_CRED_VARS.contains(&v), "{v} must be scrubbed");
        }
        for keep in [
            "ELANUS_PACKAGE",
            "ELANUS_BUS_TOKEN",
            "ELANUS_CODE_SESSION",
            "ELANUS_CODE_AGENT",
            "ELANUS_ROOT",
        ] {
            assert!(
                !PROVIDER_CRED_VARS.contains(&keep),
                "{keep} must NOT be scrubbed"
            );
        }
    }

    #[test]
    fn scrubbed_child_does_not_inherit_provider_creds_but_keeps_session_vars() {
        // The spawn-env construction the launcher uses: provider creds set on the
        // parent Command are env_remove'd before exec, while the ELANUS_* vars set
        // AFTER the scrub reach the child. We spawn a real child (`env`) through the
        // SAME `scrub_provider_creds` helper the three spawn paths use, so the test
        // exercises the actual construction, not a re-statement of it.
        use std::process::{Command, Stdio};

        let mut cmd = Command::new("/usr/bin/env");
        // The denylisted provider creds (set as elanus's .env would), INCLUDING the
        // exact DeepSeek-leak vars from the user's bug.
        cmd.env("ANTHROPIC_BASE_URL", "https://deepseek.example/x")
            .env("ANTHROPIC_API_KEY", "sk-deepseek-test")
            .env("ANTHROPIC_AUTH_TOKEN", "tok-test")
            .env("ANTHROPIC_MODEL", "deepseek-chat")
            .env("OPENAI_API_KEY", "sk-openai-test")
            .env("OPENAI_BASE_URL", "https://openai.example")
            .env("OPENAI_API_BASE", "https://openai.example/v1")
            .env("OPENAI_MODEL", "gpt-test");
        // Scrub them (the launcher's first env step), THEN set the ELANUS_* vars the
        // hook bridge / `elanus code …` children depend on (the launcher's second
        // step) — exactly the order the real spawn paths use.
        scrub_provider_creds(&mut cmd);
        cmd.env("ELANUS_PACKAGE", "code-deadbeef")
            .env("ELANUS_BUS_TOKEN", "bus-secret")
            .env(ENV_SESSION, "code-deadbeef")
            .env(ENV_AGENT, "claude-code")
            .env("ELANUS_ROOT", "/tmp/fake-root");
        cmd.stdout(Stdio::piped()).stderr(Stdio::null());

        let out = cmd.output().expect("running `env`");
        let text = String::from_utf8_lossy(&out.stdout);
        let names: std::collections::HashSet<&str> = text
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k))
            .collect();

        // The provider creds were scrubbed: the child never sees them.
        for scrubbed in PROVIDER_CRED_VARS {
            assert!(
                !names.contains(scrubbed),
                "child must NOT inherit provider cred {scrubbed}; saw env:\n{text}"
            );
        }
        // The session/bus/root vars the bridge needs survive.
        for kept in [
            "ELANUS_PACKAGE",
            "ELANUS_BUS_TOKEN",
            ENV_SESSION,
            ENV_AGENT,
            "ELANUS_ROOT",
        ] {
            assert!(
                names.contains(kept),
                "child must inherit {kept}; saw env:\n{text}"
            );
        }
    }
}
