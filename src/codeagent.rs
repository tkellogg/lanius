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
//! tool-agnostic; only the *capture mechanism* differs, and that is the free
//! adapter seam in this module (HM1 of docs/handoffs/harness-modes.md):
//!
//! - **Claude Code — a hook bridge.** The launcher inherits the child's stdio and
//!   the child's own *hooks* (a generated `--settings` config) call
//!   `elanus code hook <Event>`, which publishes. The launcher parses nothing.
//! - **Codex — a stdout stream plus a claim hook.** Headless codex runs
//!   `codex exec --json`, which prints a JSONL event stream to stdout. The launcher
//!   **pipes the child's stdout, reads it line-by-line as JSONL, maps each event,
//!   and publishes the obs record itself** (in-process, authenticating as the
//!   session principal — the same scoped-token identity). Interactive codex still
//!   imports rollout JSONL post-hoc for obs, but a generated per-session
//!   `PostToolUse` hook records live advisory apply_patch edit-claims.
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
use crate::manifest::HarnessDecl;
use crate::packages;
use crate::paths::Root;
use crate::topic;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead as _, Read as _, Write as _};
use std::path::{Path, PathBuf};

/// Env vars the launcher sets for the child coding-agent process tree, read back
/// by `elanus code hook` so each hook event publishes as the session principal.
pub const ENV_SESSION: &str = "ELANUS_CODE_SESSION";
pub const ENV_AGENT: &str = "ELANUS_CODE_AGENT";

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
const DISPATCH_HINT: &str = "[elanus] Tip: you can dispatch coding workers yourself - run `elanus code help` for all verbs. Live/blocking headless workers: `elanus code codex --headless \"<task>\"` runs a Codex worker and returns its result inline; `elanus code opencode --headless \"<task>\"` runs an opencode worker; `elanus code claude --headless \"<task>\"` runs a headless Claude worker. (Bare `elanus code <tool>` opens that tool's interactive TUI. `--worker` is the deprecated alias for `--headless`.)";

/// The session-local Claude Code skill body. Claude discovers it as a skill in the
/// per-session plugin (`build_claude_skill_plugin`) loaded via `--plugin-dir`, the
/// only channel that surfaces skills under `--setting-sources ''`. Available only
/// for this session and vanishes with the run scratch.
const ELANUS_SKILL: &str = r#"---
name: elanus
description: Shows how to dispatch coding workers from this elanus-launched Claude Code session.
---

# elanus worker dispatch

Use this cheatsheet when you need another coding worker:

- Full help: `elanus code help`
- Live/blocking Codex worker: `elanus code codex --headless "<task>"`
- Live/blocking opencode worker: `elanus code opencode --headless "<task>"`
- Live/blocking Claude worker: `elanus code claude --headless "<task>"`
  (bare `elanus code <tool>` opens the interactive TUI; `--worker` = deprecated alias for `--headless`)
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
pub fn scrub_provider_creds(cmd: &mut std::process::Command) -> &mut std::process::Command {
    for var in PROVIDER_CRED_VARS {
        cmd.env_remove(var);
    }
    cmd
}

/// Resolve the `elanus` binary used by generated hook commands.
///
/// When running from the main binary, this is just the current executable. When
/// one of the thin adapter binaries runs beside it, prefer the sibling `elanus`
/// binary in the same directory so generated hook configs still call the real
/// `elanus code hook ...` entrypoint.
fn elanus_command_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the running elanus binary")?;
    if exe.file_stem().and_then(|s| s.to_str()) == Some("elanus") {
        return Ok(exe);
    }
    if let Some(dir) = exe.parent() {
        let sibling = dir.join("elanus");
        if sibling.exists() {
            return Ok(sibling);
        }
    }
    Ok(exe)
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

/// Harness-config env vars that `PROVIDER_CRED_VARS` does NOT cover but through
/// which an inherited value could still bleed into a child (codex's `CODEX_*`,
/// opencode's `OPENCODE_CONFIG*`). When `--provider` is present, these are removed
/// BEFORE the injection is applied so a parent's harness config can't leak through
/// a nested launch (the scrub gap flagged in docs/handoffs/model-providers.md, M2).
/// This is gated on `--provider`: a no-`--provider` launch never touches them, so
/// today's behavior is byte-identical.
const HARNESS_CONFIG_VARS: &[&str] = &[
    "CODEX_API_KEY",
    "CODEX_HOME",
    "OPENCODE_CONFIG",
    "OPENCODE_CONFIG_DIR",
    "OPENCODE_CONFIG_CONTENT",
];

/// Apply a materialized provider `HarnessInjection` to a child `Command`: first
/// remove the harness-config vars an inherited value could bleed through (so a
/// parent's provider can't leak into this child — even for `NativeLogin`, whose
/// injection is empty: choosing native-login explicitly must give a clean child),
/// then set every injected env pair. Called ONLY when `--provider` is present.
/// The injection's `args` are appended by the caller at the harness-correct
/// position in argv (codex `-c` flags before the prompt); claude/opencode carry no
/// args today. Env VALUES carry the decrypted secret — they reach the child's env
/// here and are never logged (the injection's `Debug` redacts them).
fn apply_provider_injection_env(cmd: &mut std::process::Command, inj: &crate::provider::HarnessInjection) {
    for var in HARNESS_CONFIG_VARS {
        cmd.env_remove(var);
    }
    for (k, v) in &inj.env {
        cmd.env(k, v);
    }
}

/// Strip a single elanus-level `--provider <name>` that appears BEFORE the tool
/// token, returning `(Option<name>, remaining argv)`. The grammar is
/// `elanus code [--provider <name>] <tool> [tool args…]`: everything from the tool
/// token onward forwards to the tool verbatim, so scanning stops at the first
/// non-flag token (the tool). A `--provider` appearing AFTER the tool token is a
/// tool arg and is left untouched. Absent flag ⇒ argv returned unchanged (the
/// no-`--provider` invariant — byte-identical to today). Sibling of
/// `take_grants_flags`, but it runs over the WHOLE `elanus code` argv (before the
/// tool is split out) precisely because the option sits before the tool token.
pub fn take_provider_flag(args: &[String]) -> Result<(Option<String>, Vec<String>)> {
    let mut provider: Option<String> = None;
    let mut out: Vec<String> = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--provider" {
            let value = args.get(i + 1).map(|s| s.as_str()).unwrap_or("");
            if value.is_empty() || value.starts_with("--") {
                bail!("flag `--provider` requires a value but none was provided");
            }
            if provider.is_some() {
                bail!("`--provider` may be given at most once");
            }
            provider = Some(value.to_string());
            i += 2;
            continue;
        }
        // The first non-flag token is the tool; everything from here forwards
        // verbatim (a later `--provider` is a tool arg, not ours).
        if !a.starts_with('-') {
            out.extend_from_slice(&args[i..]);
            break;
        }
        // Some other pre-tool flag we don't own — keep it and keep scanning.
        out.push(args[i].clone());
        i += 1;
    }
    Ok((provider, out))
}

pub fn print_help() {
    println!(
        "\
Usage: elanus code <verb> [args...]

Launch tools (bare invocation / a prompt opens the tool's interactive TUI;
add --headless to run the captured headless worker cell instead):
  elanus code claude [args...]              launch Claude Code (TUI)
  elanus code codex [\"<task>\"]             launch Codex (TUI)
  elanus code opencode [\"<task>\"]          launch opencode (TUI)
  elanus code <tool> --headless \"<task>\"   run the tool headless and print its result
                                           (--worker is the deprecated alias for --headless)

Authority-narrowing flags (M4 — strip before tool, enforced at mint):
  --budget <N>                 turn budget for this session (u64; child ⊆ Σ≤ parent)
  --grant-publish <filter>     publish filter to grant (repeatable; MQTT wildcard OK)
  --grant-subscribe <filter>   subscribe filter to grant (repeatable)
  --grant-fs-write <path>      absolute fs-write prefix to grant (repeatable)
  --grant-fs-read <path>       absolute fs-read prefix to grant (repeatable)
  --grant-tool <name>          tool-allowlist entry to grant (repeatable)
  --grant-blocking <point>     blocking-class entry to grant (repeatable)
  Absent flags → inherit-equal from spawner (no change from before M4).
  Mint enforces child ⊆ spawner for capabilities and Σ children ≤ parent for budget.

Commands:
  elanus code deliver <worker-session> \"<message>\"  dispatch work to a worker session
  elanus code spawn <tool> \"<task>\"                  start a worker in the background
  elanus code inbox [--all] [--json]                  show this session's inbox
  elanus code resume <elanus-session> \"<message>\"    resume a recorded session
  elanus code note <session> \"<text>\"                set or clear a session note
  elanus code claim <path>                            announce an advisory edit claim
  elanus code unclaim <path>                          release an advisory edit claim
  elanus code claims [--json]                         show edit claims in this room
  elanus code whose <path> | --dirty [--json]         attribute a path (or the git-dirty set) to its owning session
  elanus code ask <session> \"<q>\" [--timeout N] [--priority N]  ask a live sibling and block for the reply
  elanus code project                                  refresh the trace->sqlite session projection
  elanus code sessions [--json]                        list coding sessions + stats
  elanus code session <id> [--json]                   one session: stats, timeline, resume command
  elanus code help                                    show this help
  elanus code list                                    list supported launch tools
  elanus code hook <event>                            internal hook bridge"
    );
}

pub fn print_tools() {
    for tool in tools() {
        println!("{tool}");
    }
}

pub fn tools() -> Vec<&'static str> {
    vec!["claude", "codex", "opencode"]
}

// Built-in coding tools resolve through seeded `[[harness]]` packages; the
// launch/resume/hook helpers below are the remaining package-agnostic utilities.

/// The launch mode of a harness *process* (harness-modes.md axis 1). Today only
/// Claude has both cells (Tui interactive vs Headless `-p`); codex/opencode are
/// Headless-only. HM2/HM3 wire the missing TUI cells and the uniform `--headless`
/// flag — HM1 just names the axis and routes today's behavior through it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// The harness's native interactive terminal UI (inherited stdio, human-pumped).
    Tui,
    /// Non-interactive, fully captured (one task in → result out).
    Headless,
}

fn harness_id_for_tool(tool: &str) -> Option<&'static str> {
    match tool {
        "claude" | "claude-code" | "cc" => Some("claude"),
        "codex" => Some("codex"),
        "opencode" | "oc" => Some("opencode"),
        _ => None,
    }
}

fn claude_agent_noun() -> &'static str {
    "claude-code"
}

fn codex_agent_noun() -> &'static str {
    "codex"
}

fn opencode_agent_noun() -> &'static str {
    "opencode"
}

/// The human-facing interactive-resume SUGGESTION for a recorded session:
/// `elanus code <tool> <passthrough…>` that re-attaches a MANAGED interactive TUI
/// to the native session, or None when the tool has no clean passthrough resume
/// (so the webui simply shows no suggestion). Surfaced by `elanus code session` and
/// the runs UI as a copy-paste hint — suggestive and per-tool; nothing core depends
/// on it. Resume itself is NOT an elanus verb: re-attaching is just a normal managed
/// launch (`elanus code claude --resume <id>`), and the daemon's async one-shot is
/// the in-process `resume_capture` primitive (M2-B), not a human command.
pub fn interactive_resume_hint(tool: &str, native_session: &str) -> Option<String> {
    if native_session.is_empty() {
        return None;
    }
    match harness_id_for_tool(tool)? {
        "claude" => Some(format!("elanus code claude --resume {native_session}")),
        _ => None,
    }
}

/// A ready-to-print " → <hint>" suffix for the resume redirect: given an elanus
/// session id, look up its record and return the per-tool interactive-resume
/// suggestion, or "" when there's no record / no clean passthrough for the tool.
/// Best-effort: any lookup error degrades to the generic redirect (empty suffix).
pub fn session_resume_hint(root: &Root, elanus_session: &str) -> String {
    if elanus_session.is_empty() {
        return String::new();
    }
    match codesession::read_record(root, elanus_session) {
        Ok(Some(rec)) => interactive_resume_hint(&rec.tool, &rec.native_session)
            .map(|cmd| format!("\n  → for {elanus_session}: `{cmd}`"))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn resolve_external_harness(root: &Root, profile_name: &str, tool: &str) -> Result<ExternalHarness> {
    find_external_harness(root, profile_name, tool)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no harness named {tool} — is it installed? (run `elanus init` to seed the stock claude/codex/opencode harness packages)"
        )
    })
}

#[derive(Debug, Clone)]
pub struct ExternalHarness {
    pub package: String,
    pub package_dir: PathBuf,
    pub decl: HarnessDecl,
}

/// Resolve a package-declared coding harness from the active profile's package
/// path. Stock harnesses are seeded as packages too, so this is now the sole
/// lookup for built-ins and external adapters alike.
pub fn find_external_harness(
    root: &Root,
    profile_name: &str,
    tool: &str,
) -> Result<Option<ExternalHarness>> {
    for pkg in packages::discover_for_profile(root, profile_name)? {
        let Some(lm) = &pkg.manifest else {
            continue;
        };
        for decl in &lm.manifest.harness {
            if decl.name == tool || decl.aliases.iter().any(|alias| alias == tool) {
                return Ok(Some(ExternalHarness {
                    package: pkg.name,
                    package_dir: pkg.dir,
                    decl: decl.clone(),
                }));
            }
        }
    }
    Ok(None)
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
    record_delivery_priority(root, requester, worker_session, message, 0).map(|(id, _corr)| id)
}

/// Like `record_delivery` but takes an `events.priority` AND returns the
/// `correlation` that threads the whole round trip — the env-free core both
/// `record_delivery` (priority 0, id-only façade) and `elanus code ask` call.
///
/// A non-zero priority rides the inbound delivery (`EmitOpts.priority`), so a
/// HIGH-priority question (`elanus code ask … --priority 5`) reaches a live sibling
/// **mid-turn** (the agent-comms HIGH-priority unseen-mail vector — Claude Code's
/// `mid_cycle_mail_injection`) rather than only on its next turn. The returned
/// correlation lets `ask` match the worker's completion reply on its OWN inbox: the
/// daemon's `route_completion` publishes the reply back to the requester's mailbox
/// carrying this SAME `correlation_id`.
pub fn record_delivery_priority(
    root: &Root,
    requester: &str,
    worker_session: &str,
    message: &str,
    priority: i64,
) -> Result<(i64, String)> {
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
            priority,
            correlation: Some(correlation.clone()),
            sender: Some(requester.clone()),
            ..crate::events::EmitOpts::new(&mailbox)
        },
    )
    .context("recording the delivery on the ledger")?;
    Ok((id, correlation))
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
pub fn spawn(root: &Root, tool: &str, prompt: &str, provider: Option<&str>) -> Result<()> {
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

    let parsed = resolve_external_harness(root, "default", tool)?;
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
    cmd.arg("code");
    // M2: forward `--provider <name>` BEFORE the tool token so the detached worker
    // wrapper re-enters `launch()` with the same provider selection (the worker
    // funnels through the same parse). Validated for existence/wire-fit in the
    // child's launch, exactly like a direct `elanus code --provider … <tool>`.
    if let Some(name) = provider {
        cmd.arg("--provider").arg(name);
    }
    cmd.arg(parsed.decl.name);
    // HM3: a detached spawn ALWAYS runs headless — a background worker has no TTY
    // and routes its completion to the spawner mailbox. Every harness now defaults
    // bare → Tui (which needs inherited stdio), so the wrapper must force the
    // uniform `--headless` flag for ALL harnesses (not just Claude as before) so
    // codex/opencode run their captured headless cell rather than trying to open a
    // TUI with detached (null) stdio.
    cmd.arg("--headless");
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
- Two independent axes. LAUNCH MODE = how a harness runs: bare \
`elanus code <tool>` (claude/codex/opencode) opens its interactive TUI; \
`elanus code <tool> --headless \"<task>\"` runs it non-interactively and captures it \
(`--worker` = deprecated alias). DRIVE PATTERN = how the result returns: live/blocking — \
run a `--headless` worker in the foreground, read its result as the command's output; or \
async — `elanus code spawn <tool> \"<task>\"` / `elanus code deliver <worker> \"<msg>\"`. \
`elanus code help` lists every verb.\n\
- For async dispatch (`spawn`/`deliver`), END YOUR TURN cleanly — do NOT poll, sleep, or \
wait; elanus wakes you later with the result. Live/blocking workers return inline.\n\
- Things addressed to you arrive as a resumed turn with the content in your prompt; \
you can also pull your own inbox with `elanus code inbox` (only YOUR mailbox). Each \
turn elanus injects an `[elanus]` note with your inbox status and any memory note. \
Prior session activity is on the bus under `obs/agent/<noun>/<session>/`.\n\
- Otherwise behave exactly as you normally would toward your human, who may or may \
not be watching this session live."
    )
}

/// Build the per-session Claude plugin carrying the bootstrap `/elanus` skill plus
/// the profile's visible skills, returning its path for `--plugin-dir`.
///
/// Why a plugin and not `.claude/skills`: Claude only discovers `.claude/skills`
/// through setting-sources that include project/user — which elanus disables with
/// `--setting-sources ''` to isolate from the user's `~/.claude`
/// (hooks/CLAUDE.md/settings). Under that isolation, `--add-dir` does NOT register
/// skills (verified empirically against claude 2.1.195). A plugin loaded via
/// `--plugin-dir` is the one channel that delivers skills BOTH under the isolation
/// AND from an arbitrary, ephemeral, per-session path:
/// `<scratch>/plugin/.claude-plugin/plugin.json` + a `skills/` dir. The bootstrap
/// skill is written as a real file (it has no source package); the profile's skills
/// are SYMLINKED in (live, no copy). The whole scratch is `remove_dir_all`'d at
/// launch exit, so the plugin is ephemeral and private to the session.
fn build_claude_skill_plugin(scratch: &Path, skills: &[(String, PathBuf)]) -> Result<PathBuf> {
    let plugin = scratch.join("plugin");
    let manifest_dir = plugin.join(".claude-plugin");
    std::fs::create_dir_all(&manifest_dir)
        .with_context(|| format!("creating plugin manifest dir {}", manifest_dir.display()))?;
    std::fs::write(
        manifest_dir.join("plugin.json"),
        r#"{"name":"elanus","version":"1.0.0","description":"elanus session skills"}"#,
    )
    .with_context(|| "writing the elanus plugin manifest".to_string())?;
    // The bootstrap dispatch skill — a real file, it has no source package.
    let skills_dir = plugin.join("skills");
    let elanus_skill = skills_dir.join("elanus");
    std::fs::create_dir_all(&elanus_skill)
        .with_context(|| format!("creating elanus skill dir {}", elanus_skill.display()))?;
    let skill_path = elanus_skill.join("SKILL.md");
    std::fs::write(&skill_path, ELANUS_SKILL)
        .with_context(|| format!("writing {}", skill_path.display()))?;
    // The profile's visible skills, symlinked alongside it (best-effort per skill).
    link_skill_packages(&skills_dir, skills)?;
    Ok(plugin)
}

/// Take the `--profile <name>` launch flag: the elanus profile whose VISIBLE
/// skills this coding session adopts (the same `discover_for_profile ∩
/// skill_visible` set the native renderer uses, `render.rs`). Default `"default"`
/// — every `elanus code` session materializes the default profile's skills, the
/// same whole-system config home native agents read. The flag and its value are
/// stripped before the args reach the tool. A bare trailing `--profile` (no value)
/// is ignored (keeps the default). Returns `(profile_name, filtered_args)`.
///
/// NB: elanus consumes the LONG `--profile`; Codex's own config-profile is still
/// reachable via its short `-p` form, which passes through to codex untouched.
fn take_profile_flag(args: &[String]) -> (String, Vec<String>) {
    let mut profile: Option<String> = None;
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--profile" {
            if let Some(v) = args.get(i + 1) {
                let v = v.trim();
                if !v.is_empty() {
                    profile = Some(v.to_string());
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
    (profile.unwrap_or_else(|| "default".to_string()), out)
}

/// The visible skill packages for `profile_name`: exactly the
/// `discover_for_profile ∩ skill_visible ∩ has-SKILL.md` set the native renderer
/// surfaces (`render.rs` §3), as `(name, dir)` pairs. Best-effort — a profile-load
/// or discovery error logs and yields an empty set, so a coding launch is never
/// fatal-blocked on its skills (it just gets the bootstrap `/elanus` skill alone).
fn visible_skill_packages(root: &Root, profile_name: &str) -> Vec<(String, PathBuf)> {
    let prof = match crate::profile::load(root, profile_name) {
        Ok((p, _)) => p,
        Err(e) => {
            eprintln!(
                "[code] loading profile {profile_name:?} for skills ({e:#}); \
                 no profile skills materialized"
            );
            return Vec::new();
        }
    };
    match crate::packages::discover_for_profile(root, profile_name) {
        Ok(pkgs) => pkgs
            .into_iter()
            .filter(|p| p.meta.is_some() && crate::profile::skill_visible(&prof, &p.name))
            .map(|p| (p.name, p.dir))
            .collect(),
        Err(e) => {
            eprintln!("[code] discovering skills for profile {profile_name:?}: {e:#}");
            Vec::new()
        }
    }
}

/// Symlink each skill package dir into `skills_dir` as `<skills_dir>/<name>` — the
/// `<name>/SKILL.md` shape every harness's skills loader scans (Claude Code's
/// `.claude/skills`, Codex's `$CODEX_HOME/skills`, opencode's
/// `$OPENCODE_CONFIG_DIR/skills`). One operation, three harnesses; only the parent
/// dir differs. A symlink (not a copy) so edits to the source package reflect live
/// and skill-relative script paths resolve back into the real package tree; the
/// per-session run scratch this lands in is `remove_dir_all`'d at launch exit, so
/// the links are ephemeral and private to the session. Best-effort per skill: a
/// single bad link is logged, not fatal. No-op (no dir created) for an empty set.
fn link_skill_packages(skills_dir: &Path, skills: &[(String, PathBuf)]) -> Result<()> {
    if skills.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(skills_dir)
        .with_context(|| format!("creating skills dir {}", skills_dir.display()))?;
    for (name, dir) in skills {
        let link = skills_dir.join(name);
        if let Err(e) = std::os::unix::fs::symlink(dir, &link) {
            eprintln!(
                "[code] linking skill {name} -> {} into {}: {e:#}",
                dir.display(),
                skills_dir.display()
            );
        }
    }
    Ok(())
}

/// Build a per-session, ephemeral `CODEX_HOME` carrying elanus's managed codex
/// hooks and, when present, the profile's skills. codex 0.141.0 scans
/// `$CODEX_HOME/skills` but has no per-invocation skills lever, and its hook config
/// lives in `config.toml`; an isolated home is therefore the scoped way to add both
/// without writing into the user's real `~/.codex` or the repo. The user's real
/// auth/version stay symlinked (secret read in place, never copied), while
/// `config.toml` is copied then appended with the elanus PostToolUse hook.
fn build_codex_skills_home(
    root: &Root,
    session: &str,
    skills: &[(String, PathBuf)],
) -> Result<PathBuf> {
    let home = root.run_dir().join(session).join("codex_home");
    std::fs::create_dir_all(&home)
        .with_context(|| format!("creating codex home {}", home.display()))?;

    // Mirror the user's real codex auth/version by symlink so codex authenticates
    // exactly as it would unredirected. `config.toml` is copied below because we
    // append managed hooks to the session-local copy.
    let dst_config = home.join("config.toml");
    let _ = std::fs::remove_file(&dst_config);
    if let Some(real) = dirs_next_home_codex() {
        for entry in ["auth.json", "version.json"] {
            let src = real.join(entry);
            if src.exists() {
                let dst = home.join(entry);
                let _ = std::fs::remove_file(&dst);
                if let Err(e) = std::os::unix::fs::symlink(&src, &dst) {
                    eprintln!(
                        "[code] linking codex {entry} {} -> {}: {e:#}",
                        dst.display(),
                        src.display()
                    );
                }
            }
        }

        let real_config = real.join("config.toml");
        if real_config.exists() {
            std::fs::copy(&real_config, &dst_config).with_context(|| {
                format!(
                    "copying codex config {} -> {}",
                    real_config.display(),
                    dst_config.display()
                )
            })?;
        }
    }
    append_codex_hook_config(root, &home)?;
    link_skill_packages(&home.join("skills"), skills)
        .with_context(|| format!("linking skills into codex home {}", home.display()))?;
    Ok(home)
}

fn append_codex_hook_config(root: &Root, home: &Path) -> Result<()> {
    let self_exe = elanus_command_path()?;
    let config = home.join("config.toml");
    let needs_separator = config.metadata().map(|m| m.len() > 0).unwrap_or(false);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config)
        .with_context(|| format!("opening codex config {}", config.display()))?;
    if needs_separator {
        f.write_all(b"\n\n")?;
    }
    f.write_all(codex_hook_config(&self_exe, root).as_bytes())?;
    Ok(())
}

fn codex_hook_config(self_exe: &Path, root: &Root) -> String {
    let exe = self_exe.display().to_string();
    let root_dir = std::fs::canonicalize(&root.dir).unwrap_or_else(|_| root.dir.clone());
    let root_arg = root_dir.display().to_string();
    let command = format!("{exe} -C {root_arg} code hook PostToolUse");
    format!(
        "[[hooks.PostToolUse]]\nmatcher = \"*\"\n[[hooks.PostToolUse.hooks]]\ntype = \"command\"\ncommand = \"{}\"\n",
        toml_basic_string_content(&command)
    )
}

fn toml_basic_string_content(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

/// The user's real codex home (`$CODEX_HOME`, else `~/.codex`) — the source of the
/// auth/config entries the per-session skills home mirrors. None if no home dir is
/// resolvable (then the per-session home carries skills but no mirrored auth, and
/// codex falls back to its own discovery).
fn dirs_next_home_codex() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("CODEX_HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex"))
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

/// Should the harness launch in HEADLESS mode? (HM3) The uniform `--headless`
/// flag anywhere in the user args selects the harness's non-interactive cell
/// (`claude -p` / `codex exec --json` / `opencode run --format json`), captures
/// the result, and prints a marked result for a parent agent to read. `--worker`
/// is the DEPRECATED ALIAS (the pre-HM3 spelling) and still works, with a one-line
/// stderr deprecation notice. Either flag is stripped before the real tool sees
/// argv, matching the other elanus-only launch flags.
fn take_headless_flag(args: &[String]) -> (bool, Vec<String>) {
    let mut headless = false;
    let mut saw_worker_alias = false;
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        if a == "--headless" {
            headless = true;
        } else if a == "--worker" {
            headless = true;
            saw_worker_alias = true;
        } else {
            out.push(a.clone());
        }
    }
    if saw_worker_alias {
        eprintln!("[code] note: --worker is deprecated; use --headless (it still works for now)");
    }
    (headless, out)
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

/// Pull M4 grant-narrowing flags out of the user args, validate them, and return
/// the populated `RequestedGrants` plus the remaining args (which are forwarded
/// to the tool untouched, just like the other take_*_flag helpers).
///
/// Flags recognised (each is an elanus-only flag stripped before the tool sees
/// argv):
///
/// - `--budget <N>`           → `budget: Some(N)` (u64; rejects non-numeric)
/// - `--grant-publish <filter>`  (repeatable) → `publish`
/// - `--grant-subscribe <filter>` (repeatable) → `subscribe`
/// - `--grant-fs-write <path>` (repeatable)  → `fs_write` (absolute, non-empty)
/// - `--grant-fs-read <path>`  (repeatable)  → `fs_read`  (absolute, non-empty)
/// - `--grant-tool <name>`    (repeatable)   → `tool_allowlist` (non-empty)
/// - `--grant-blocking <pt>`  (repeatable)   → `blocking`       (non-empty)
///
/// A flag present with no value, or an invalid value, is a hard usage error
/// (bail! naming the flag and what is wrong). Absent flags leave the
/// corresponding `RequestedGrants` field as `None` (inherit-equal).
///
/// Security note (docs/security.md entry 22 [M3 lesson]): fs path prefixes are
/// validated absolute + non-empty here, at construction, so an empty/relative
/// prefix — which would be a silent root-wildcard footgun in `path_covered` —
/// is rejected before it reaches `mint`.
fn take_grants_flags(args: &[String]) -> anyhow::Result<(codesession::RequestedGrants, Vec<String>)> {
    let mut budget: Option<u64> = None;
    let mut publish: Vec<String> = Vec::new();
    let mut subscribe: Vec<String> = Vec::new();
    let mut fs_write: Vec<String> = Vec::new();
    let mut fs_read: Vec<String> = Vec::new();
    let mut tool_allowlist: Vec<String> = Vec::new();
    let mut blocking: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::with_capacity(args.len());

    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        // Value-taking flags: consume flag + next token, reject if next token is missing.
        match flag {
            "--budget" | "--grant-publish" | "--grant-subscribe"
            | "--grant-fs-write" | "--grant-fs-read"
            | "--grant-tool" | "--grant-blocking" => {
                let value = args
                    .get(i + 1)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                // A bare trailing flag (no value) or a value that itself looks like a
                // flag is a usage error.
                if value.is_empty() || value.starts_with("--") {
                    anyhow::bail!(
                        "flag `{flag}` requires a value but none was provided"
                    );
                }
                match flag {
                    "--budget" => {
                        let n: u64 = value.parse().map_err(|_| {
                            anyhow::anyhow!(
                                "--budget requires a non-negative integer, got {:?}",
                                value
                            )
                        })?;
                        budget = Some(n);
                    }
                    "--grant-publish" => {
                        if !crate::topic::valid_filter(value) {
                            anyhow::bail!(
                                "--grant-publish {:?} is not a valid MQTT topic filter \
                                 (wildcards: + per-level, # only at end)",
                                value
                            );
                        }
                        publish.push(value.to_string());
                    }
                    "--grant-subscribe" => {
                        if !crate::topic::valid_filter(value) {
                            anyhow::bail!(
                                "--grant-subscribe {:?} is not a valid MQTT topic filter",
                                value
                            );
                        }
                        subscribe.push(value.to_string());
                    }
                    "--grant-fs-write" => {
                        validate_fs_grant_path("--grant-fs-write", value)?;
                        fs_write.push(value.to_string());
                    }
                    "--grant-fs-read" => {
                        validate_fs_grant_path("--grant-fs-read", value)?;
                        fs_read.push(value.to_string());
                    }
                    "--grant-tool" => {
                        if value.is_empty() {
                            anyhow::bail!("--grant-tool requires a non-empty tool name");
                        }
                        tool_allowlist.push(value.to_string());
                    }
                    "--grant-blocking" => {
                        if value.is_empty() {
                            anyhow::bail!("--grant-blocking requires a non-empty blocking-class name");
                        }
                        blocking.push(value.to_string());
                    }
                    _ => unreachable!(),
                }
                i += 2;
                continue;
            }
            _ => {
                out.push(args[i].clone());
            }
        }
        i += 1;
    }

    let grants = codesession::RequestedGrants {
        budget,
        publish: if publish.is_empty() { None } else { Some(publish) },
        subscribe: if subscribe.is_empty() { None } else { Some(subscribe) },
        fs_write: if fs_write.is_empty() { None } else { Some(fs_write) },
        fs_read: if fs_read.is_empty() { None } else { Some(fs_read) },
        tool_allowlist: if tool_allowlist.is_empty() { None } else { Some(tool_allowlist) },
        blocking: if blocking.is_empty() { None } else { Some(blocking) },
    };
    Ok((grants, out))
}

/// Validate a filesystem path supplied to a `--grant-fs-*` flag:
/// must be non-empty and absolute. An empty or relative prefix is the
/// root-wildcard footgun `path_covered` caught (security.md entry 22 [M3]).
fn validate_fs_grant_path(flag: &str, path: &str) -> anyhow::Result<()> {
    use std::path::{Component, Path};
    if path.is_empty() {
        anyhow::bail!("{flag} requires a non-empty path");
    }
    // Reject leading/trailing whitespace — almost always a config/split artifact,
    // and " /x" / "/x " are silently different from the intended prefix.
    if path != path.trim() {
        anyhow::bail!("{flag} {path:?} has leading/trailing whitespace");
    }
    if !Path::new(path).is_absolute() {
        anyhow::bail!(
            "{flag} {:?} must be an absolute path (a relative prefix would be a \
             root-wildcard footgun; supply an absolute prefix like /home/user/project)",
            path
        );
    }
    // Deny degenerate absolutes that are lexically root-or-escaping. `is_absolute`
    // alone admits `/`, `//`, `/.`, `/../..` — each normalizes to (or escapes
    // toward) the filesystem root, i.e. a near-root grant (the M3 root-wildcard
    // footgun's cousin). Mirror path_covered's deny-when-degenerate posture: a
    // grant must name at least one real directory below root and must not contain
    // `..`. "Unbounded" is expressed by OMITTING the flag (None), never `/`.
    let mut normal_segments = 0usize;
    for comp in Path::new(path).components() {
        match comp {
            Component::ParentDir => anyhow::bail!(
                "{flag} {path:?} contains `..` — supply a fully-resolved absolute \
                 prefix (no parent traversal)"
            ),
            Component::Normal(_) => normal_segments += 1,
            _ => {} // RootDir / CurDir / Prefix
        }
    }
    if normal_segments == 0 {
        anyhow::bail!(
            "{flag} {path:?} resolves to the filesystem root — refusing a near-root \
             grant; name a real directory below root, or omit the flag to inherit \
             (None = unbounded)"
        );
    }
    Ok(())
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

// ── SI4 / sibling-resolution: `whose` (attribution) + `ask` (deliver-and-wait) ─
//
// The CLI surface of docs/handoffs/sibling-intent.md (SI4) and
// docs/handoffs/sibling-resolution-skills.md. `whose` answers "which of these
// dirty files are mine, and who owns the rest?" by mapping a path (or the whole
// `git status` set) to its owning session via `codesession::whose_path`. `ask` is a
// blocking deliver-and-wait: send a scoped question to a live sibling and block
// briefly for the correlated reply, so an agent need not hand-roll the poll loop.

/// SI1 (sibling-intent): render an RFC3339 timestamp as a humanized "time since"
/// delta for the sibling note + `whose`/`ask`. "just now" / "30s ago" / "4m ago" /
/// "1h ago" / "2d ago". An unparseable/empty timestamp degrades to "recently" — the
/// note is advisory, so we never fabricate a precision we don't have.
fn humanize_since(rfc3339: &str) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(rfc3339.trim()) else {
        return "recently".to_string();
    };
    let secs = (chrono::Utc::now() - then.with_timezone(&chrono::Utc)).num_seconds();
    if secs < 5 {
        "just now".to_string()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// SI2 (sibling-intent): clip a task text for the compact sibling note / `whose`
/// line (~80 chars, plain ellipsis — `clip`'s "[clipped N chars]" tail is too noisy
/// for an inline task). Whitespace-trimmed so a wrapped todo renders on one line.
fn clip_task(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 80;
    if text.chars().count() <= MAX {
        text
    } else {
        let head: String = text.chars().take(MAX).collect();
        format!("{head}…")
    }
}

/// `elanus code ask <session> "<question>" [--timeout SECS] [--priority N] [--json]`
/// — a blocking deliver-and-wait (sibling-resolution skills "ask-sibling"). Sends
/// the question to `<session>` threaded on a fresh correlation, then BLOCKS up to
/// `--timeout` (default 20s) polling THIS session's OWN inbox (~1/s) for the
/// correlated reply. Prints the reply on arrival; on timeout prints "no answer …
/// treat the contended file as theirs" and returns Ok (silence is a legitimate
/// answer — route around it, don't error). `--priority N` rides the delivery so a
/// HIGH-priority question can reach the sibling mid-turn. Identity comes ONLY from
/// the env the launcher set (never an argument), the same own-inbox-only guarantee
/// `inbox` has.
pub fn ask_cmd(root: &Root, args: &[String]) -> Result<()> {
    let own_session = std::env::var(ENV_SESSION).ok().filter(|s| !s.is_empty());
    let own_noun = std::env::var(ENV_AGENT).ok().filter(|s| !s.is_empty());
    let (Some(own_session), Some(own_noun)) = (own_session, own_noun) else {
        bail!(
            "elanus code ask must run inside a coding session \
             (no {ENV_SESSION}/{ENV_AGENT} in the environment — run it from a \
             session launched by `elanus code`)"
        );
    };

    // Parse: first positional = target session; remaining positionals = the
    // question; flags --timeout/--priority/--json may appear anywhere.
    let mut target = String::new();
    let mut words: Vec<String> = Vec::new();
    let mut timeout_secs: u64 = 20;
    let mut priority: i64 = 0;
    let mut want_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout" => {
                i += 1;
                timeout_secs = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("--timeout needs a number of seconds"))?;
            }
            "--priority" => {
                i += 1;
                priority = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("--priority needs an integer"))?;
            }
            "--json" => want_json = true,
            other if target.is_empty() => target = other.to_string(),
            other => words.push(other.to_string()),
        }
        i += 1;
    }
    let question = words.join(" ");
    if target.is_empty() || question.trim().is_empty() {
        bail!(
            "usage: elanus code ask <session> \"<question>\" [--timeout SECS] [--priority N] [--json]"
        );
    }

    // Send the question threaded on a FRESH correlation, captured so we can match
    // the worker's completion reply on our own inbox (route_completion echoes it).
    let (_id, correlation) =
        record_delivery_priority(root, &own_session, &target, &question, priority)?;
    eprintln!(
        "[code] asked {target} (corr {correlation}); waiting up to {timeout_secs}s for a reply"
    );

    // BLOCK: poll our own inbox ~1/s for a delivery carrying our correlation.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        // Best-effort: a transient inbox read error just retries next tick.
        if let Ok(items) = codesession::inbox_for_session(root, &own_noun, &own_session, false) {
            if let Some(reply) = items
                .into_iter()
                .find(|it| it.correlation.as_deref() == Some(correlation.as_str()))
            {
                if want_json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "answered": true,
                            "from": reply.from,
                            "correlation": reply.correlation,
                            "message": reply.message,
                        }))?
                    );
                } else {
                    let from = reply.from.as_deref().unwrap_or(target.as_str());
                    println!("{from} answered: {}", clip(&reply.message, 4000));
                }
                // Mark it seen so it doesn't re-surface in the per-turn inbox count.
                let _ = codesession::mark_inbox_seen(root, &own_session, &[reply.event_id]);
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    if want_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "answered": false,
                "session": target,
                "timeout_secs": timeout_secs,
            }))?
        );
    } else {
        println!(
            "no answer from {target} within {timeout_secs}s — treat the contended file as theirs"
        );
    }
    Ok(())
}

/// `elanus code whose <path>` / `elanus code whose --dirty [--json]` — change
/// attribution (SI4). Maps a path (or the whole `git status --porcelain` set) to its
/// owning coding session via `codesession::whose_path`, printing the owner, its
/// tool, humanized last-active, and current task. A path no session claims reads as
/// "unattributed" (likely the viewer's own work, or untracked-by-elanus).
pub fn whose_cmd(root: &Root, args: &[String]) -> Result<()> {
    let want_json = args.iter().any(|a| a == "--json");
    let want_dirty = args.iter().any(|a| a == "--dirty");
    let viewer = std::env::var(ENV_SESSION).ok().filter(|s| !s.is_empty());
    let viewer = viewer.as_deref();

    if want_dirty {
        // Annotate the whole working-tree change set. `git status --porcelain` in
        // the CWD (std::process::Command — no shell), one attribution per path.
        let out = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .output()
            .context("running `git status --porcelain` (is this a git repo?)")?;
        if !out.status.success() {
            bail!(
                "`git status --porcelain` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let paths = parse_porcelain_paths(&text);
        if want_json {
            let arr: Vec<Value> = paths.iter().map(|p| whose_json(root, p, viewer)).collect();
            println!("{}", serde_json::to_string_pretty(&json!(arr))?);
        } else if paths.is_empty() {
            println!("(working tree clean — nothing to attribute)");
        } else {
            for p in &paths {
                println!("{}", whose_line(root, p, viewer));
            }
        }
        return Ok(());
    }

    // Single-path form: the first non-flag argument.
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        bail!("usage: elanus code whose <path>   |   elanus code whose --dirty [--json]");
    };
    if want_json {
        println!("{}", serde_json::to_string_pretty(&whose_json(root, path, viewer))?);
    } else {
        println!("{}", whose_line(root, path, viewer));
    }
    Ok(())
}

/// Extract the changed-file paths from `git status --porcelain` output. Each line is
/// `XY <path>` (cols 0..2 are the status code, col 2 a space, the path from col 3);
/// a rename is `XY <old> -> <new>` — we take the post-rename path. Git quotes a path
/// with special chars, so a surrounding pair of quotes is stripped. Bytes 0..3 are
/// always ASCII, so the slice never splits a multibyte char.
fn parse_porcelain_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in text.lines() {
        if line.len() < 4 {
            continue;
        }
        let rest = line[3..].trim();
        let path = match rest.rsplit_once(" -> ") {
            Some((_old, new)) => new,
            None => rest,
        };
        let path = path.trim().trim_matches('"');
        if !path.is_empty() {
            paths.push(path.to_string());
        }
    }
    paths
}

/// One human-readable attribution line for `whose`.
fn whose_line(root: &Root, path: &str, viewer: Option<&str>) -> String {
    match codesession::whose_path(root, path) {
        Some(att) => {
            let since = humanize_since(&att.last_active);
            let task = att
                .current_task
                .as_deref()
                .map(|t| format!(" — {}", clip_task(t)))
                .unwrap_or_default();
            let mine = viewer == Some(att.session.as_str());
            let yours = if mine { ", yours" } else { "" };
            format!(
                "{path}  ← {} ({}, last active {since}{yours}){task}",
                att.session, att.agent_noun
            )
        }
        None => format!("{path}  ← unattributed (no session claims it — likely yours)"),
    }
}

/// One JSON attribution object for `whose --json`.
fn whose_json(root: &Root, path: &str, viewer: Option<&str>) -> Value {
    match codesession::whose_path(root, path) {
        Some(att) => json!({
            "path": path,
            "attributed": true,
            "session": att.session,
            "tool": att.agent_noun,
            "last_active": att.last_active,
            "last_active_human": humanize_since(&att.last_active),
            "current_task": att.current_task,
            "mine": viewer == Some(att.session.as_str()),
        }),
        None => json!({
            "path": path,
            "attributed": false,
            "session": Value::Null,
            "mine": Value::Null,
        }),
    }
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
    // Canonicalize to the SAME absolute form auto_claim_write uses (BUG B): a manual
    // `claim src/foo.rs` and the SA3 auto-claim of that file must collapse to ONE row
    // per session, not a lexical row plus a canonical one double-listing it for a
    // roommate. A relative manual path resolves against the session's recorded
    // workdir, mirroring auto-claim's base. Fall back to the trimmed input if the
    // record/workdir is somehow unavailable (still advisory, never a panic).
    let workdir = session_auto_claim_room_and_workdir(root, &session).map(|(_, wd)| wd);
    let claim_path =
        canonicalize_claim_path(path, workdir.as_deref()).unwrap_or_else(|| path.to_string());
    codesession::add_claim(root, &room, &session, &claim_path)?;
    println!(
        "claimed {claim_path} in room {room} (advisory — your peers will see you are \
editing it; nothing is locked)"
    );
    Ok(())
}

// ── SA3 (write half): touching a file IS the claim ───────────────────────────
//
// docs/handoffs/sibling-awareness.md SA3 + coding-agent-tails.md SA3. The
// acceptance: an agent that EDITS a file without ever running `elanus code claim`
// must still appear, for that path, in a roommate's `claims` and per-turn
// injection — so coordination stops depending on an agent remembering to claim.
//
// SOURCE DECISION (settled): we DO NOT drive auto-claims from an `obs/fs/#`
// subscriber. The obs/fs WRITE camera (src/exec.rs emit_fs_delta) only brackets
// CAGED actors (the kernel shell/exec + package actors via Cage::shell_command /
// Cage::command). Coding agents (claude/codex/opencode) are NOT in elanus's cage
// — each keeps its own tool sandbox; the cage bypass is a deferred milestone
// (coding-agents.md) — so an obs/fs subscriber would NEVER witness a coding
// agent's edits and would NOT satisfy SA3's acceptance. Instead we auto-claim
// from each coding agent's OWN file-write TOOL events — the same per-session
// capture/projection locus where read-provenance M1 projects Read/Grep/Glob
// (the Claude PreToolUse hook) and where codex/opencode already harvest changed
// paths (codex_collect_summary `file_change`, opencode_collect_summary
// edit|write). This is the honest-agent tier and it meets SA3's acceptance for
// all three harnesses NOW; the authoritative cage-based version arrives with the
// coding-agent cage bypass (then read-provenance M2 / SA3's READ half follow).
//
// Advisory, NEVER authorization (homogeneous-authority doctrine, security.md):
// the auto-claim is information a sibling routes around — it never blocks a
// write, gates nothing, mints/checks no token. Dedupe is structural: add_claim
// upserts per (room, session, path) PRIMARY KEY, so re-editing the same file just
// refreshes the timestamp — never a duplicate, never per-syscall.

/// Resolve the room an auto-claim for `session` lands in AND the session's durable
/// recorded workdir, both read from the session's OWN record in a single load. The
/// room is the explicit `--room` recorded at launch if any, else the SA1
/// workdir-derived room from the recorded canonical workdir — the SAME resolution
/// `session_room_identity` uses for the `claim` CLI, so the auto-claim lands in the
/// room siblings read. The workdir is returned so a relative claim path can be
/// resolved against it even on a RESUMED session, where the live `cwd` is `None`
/// (BUG A): resume then claims the same absolute path launch would.
fn session_auto_claim_room_and_workdir(root: &Root, session: &str) -> Option<(String, String)> {
    let rec = codesession::read_record(root, session).ok().flatten()?;
    let workdir = rec.workdir.clone();
    let room = match rec.room.filter(|r| !r.is_empty()) {
        Some(r) => r,
        None => resolve_room(None, Path::new(&workdir)),
    };
    Some((room, workdir))
}

/// Resolve a claim path to its canonical absolute form, the SAME convention the fs
/// cameras key on (emit_fs_delta / claude_read_fs_events both key
/// `obs/fs/<canonical>`) — so a MANUAL `claim` and an AUTO-claim of one file
/// collapse to ONE row per session (BUG B), and an auto-claim matches a roommate's
/// view. A relative `raw_path` is resolved against `base` (the tool's live cwd when
/// known, else the session's recorded workdir), then canonicalized best-effort,
/// falling back to the joined lexical path when canonicalize fails (e.g. a file the
/// write just created then removed still keys deterministically). `base` empty/None
/// leaves a relative path lexical. Returns `None` only for a blank input.
fn canonicalize_claim_path(raw_path: &str, base: Option<&str>) -> Option<String> {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() {
        return None;
    }
    let p = PathBuf::from(raw_path);
    let abs = if p.is_absolute() {
        p
    } else if let Some(b) = base.filter(|b| !b.is_empty()) {
        Path::new(b).join(&p)
    } else {
        p
    };
    let canon = std::fs::canonicalize(&abs).unwrap_or(abs);
    Some(canon.to_string_lossy().into_owned())
}

/// SA3 write-half mechanism: record an advisory edit-claim for `session` on the
/// path it just wrote via a file-write tool event, in the session's room. Called
/// from each harness's write-tool detection (claude PreToolUse Write/Edit/...,
/// codex `file_change`, opencode `edit`/`write`). Canonicalizes the path to an
/// absolute, canonical form (resolving a relative path against `cwd` when given)
/// so the claimed path matches the convention the fs cameras and a roommate's view
/// use — NOT the raw lexical string. A blank/empty path is a no-op (never panics,
/// never claims an empty path). ADVISORY: every failure is swallowed (logged at
/// most) — recording a claim must never break or block the coding session.
pub fn auto_claim_write(root: &Root, session: &str, raw_path: &str, cwd: Option<&str>) {
    if raw_path.trim().is_empty() {
        // No/blank path (e.g. a malformed tool event): nothing honest to claim.
        return;
    }
    let Some((room, workdir)) = session_auto_claim_room_and_workdir(root, session) else {
        // No durable record yet (native session id not observed) — can't resolve
        // the room. Skip silently; the next write after the record lands will claim.
        return;
    };
    // Resolve a relative path against the tool's LIVE cwd when known, else fall back
    // to the session's recorded workdir (BUG A): on a RESUMED codex/opencode session
    // capture is called with record_workdir=None → cwd=None here, and canonicalizing
    // a relative path against the launcher's process cwd would yield a WRONG claim.
    // The recorded workdir is the same base launch resolved against, so resume now
    // claims the identical absolute path. An absolute path ignores the base.
    let base = cwd.filter(|c| !c.is_empty()).unwrap_or(workdir.as_str());
    let Some(claim_path) = canonicalize_claim_path(raw_path, Some(base)) else {
        return;
    };
    // add_claim is idempotent per (room, session, path): re-editing the same file
    // refreshes the timestamp, never duplicates — the dedupe guarantee SA3 needs.
    if let Err(e) = codesession::add_claim(root, &room, session, &claim_path) {
        eprintln!("[code] auto-claim (advisory, continuing): {e:#}");
    }
}

/// The Claude-Code tool names that write a single file via a `file_path` input.
/// MultiEdit edits ONE file (multiple hunks) and still carries a single top-level
/// `file_path`; NotebookEdit carries `notebook_path`. These are the SA3 write
/// signals on the same PreToolUse hook that M1 reads for Read/Grep/Glob.
fn claude_write_tool_path<'a>(tool: &str, input: Option<&'a Value>) -> Option<&'a str> {
    let key = match tool {
        "Write" | "Edit" | "MultiEdit" => "file_path",
        "NotebookEdit" => "notebook_path",
        _ => return None,
    };
    input
        .and_then(|i| i.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
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
/// The injection VECTORS a memory block can reach a coding session through (M4).
/// Ordered from quietest to loudest; a harness that cannot do a louder vector
/// DEGRADES down this ladder (see `achievable_vector`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionVector {
    /// Lands at the start of the next turn (all harnesses, today).
    NextTurn,
    /// Lands BETWEEN tool calls, mid-turn (Claude Code via Pre/PostToolUse
    /// `additionalContext`; spike-proven). opencode served/TUI is a NOTED future
    /// path (deferred). Codex has no live hook bridge → degrades to next-turn.
    MidCycle,
}

/// M4 capability matrix — given a harness agent noun and the DESIRED vector for a
/// block, return the vector that harness can actually achieve, degrading down the
/// ladder rather than erroring or dropping. This is the per-(harness, capability)
/// shape the harness-modes work uses, kept tiny and total:
///
/// | harness            | next-turn | mid-cycle                       |
/// |--------------------|-----------|---------------------------------|
/// | claude-code        | yes       | yes (Pre/PostToolUse hook)      |
/// | codex              | yes       | DEGRADES → next-turn (no hooks) |
/// | opencode           | yes       | DEGRADES → next-turn (headless)*|
///
/// *opencode SERVED/TUI mid-cycle (server `prompt_async`) is a real future vector
/// (see the spike) but is DEFERRED — M4 ships next-turn-everywhere + Claude-Code
/// mid-cycle. Until then opencode degrades mid-cycle to next-turn, same as Codex.
pub fn achievable_vector(agent_noun: &str, desired: InjectionVector) -> InjectionVector {
    match desired {
        InjectionVector::NextTurn => InjectionVector::NextTurn,
        InjectionVector::MidCycle => {
            if agent_noun == "claude-code" {
                InjectionVector::MidCycle
            } else {
                // Codex (no live hook bridge) and opencode-headless (no served
                // control plane wired for blocks yet) cannot push mid-cycle →
                // degrade to next-turn. The caller logs the downgrade legibly.
                InjectionVector::NextTurn
            }
        }
    }
}

/// Load the durable memory blocks to render in this coding session's per-turn
/// injection: its agent-noun-owned agent-scope blocks + its session-scope blocks,
/// ordered by priority — EXCLUDING the well-known `note` block (rendered separately
/// as `[elanus note]`). Best-effort: a ledger error yields no blocks rather than
/// breaking the (telemetry-tier) injection.
fn session_memory_blocks(
    root: &Root,
    agent_noun: &str,
    session: &str,
) -> Vec<crate::context_store::LoadedBlock> {
    let Ok(conn) = crate::db::open(root) else {
        return Vec::new();
    };
    if crate::db::init_schema(&conn).is_err() {
        return Vec::new();
    }
    crate::context_store::load_session_blocks(&conn, agent_noun, session)
        .unwrap_or_default()
        .into_iter()
        .filter(|b| b.name != crate::context_store::NOTE_BLOCK)
        .collect()
}

/// M4 — compose the mid-cycle injection text (Claude Code): the pending,
/// not-yet-delivered mid-cycle blocks for this session, rendered in the same
/// `[elanus block: …]` shape the next-turn vector uses, bracketed so the model
/// reads them as a system note. `None` when nothing is pending (the dedup'd common
/// case), so a quiet tool call emits nothing. MUTATES the dedup table — call only
/// from the emitting hook arm.
fn mid_cycle_injection(root: &Root, agent_noun: &str, session: &str) -> Option<String> {
    let conn = crate::db::open(root).ok()?;
    crate::db::init_schema(&conn).ok()?;
    let pending = crate::context_store::take_pending_mid_cycle(&conn, agent_noun, session)
        .unwrap_or_default();
    if pending.is_empty() {
        return None;
    }
    let mut out = String::from(
        "[elanus] Urgent context delivered mid-task (priority block(s) — read before continuing):",
    );
    for b in &pending {
        out.push_str(&format!(
            "\n[elanus block: {}] {}",
            b.name,
            clip(&b.content, 2000)
        ));
    }
    Some(out)
}

/// C3 (agent-comms) — compose the mid-cycle injection text for HIGH-priority
/// UNSEEN inbox mail (Claude Code): the not-yet-delivered-mid-cycle messages whose
/// `events.priority >= high_priority_threshold` (config). `None` when there is no
/// such mail (the dedup'd common case). MUTATES the dedup table
/// (`code_mail_delivered`) — call only from the emitting hook arm. The message is
/// NOT marked seen: it still shows in the next-turn inbox block until the agent
/// pulls it; this vector only makes the urgent ones arrive sooner, once each.
fn mid_cycle_mail_injection(root: &Root, agent_noun: &str, session: &str) -> Option<String> {
    let threshold = high_priority_threshold(root);
    let pending = codesession::take_pending_mid_cycle_mail(root, agent_noun, session, threshold)
        .unwrap_or_default();
    if pending.is_empty() {
        return None;
    }
    let mut out = String::from(
        "[elanus] Urgent mail arrived mid-task (high-priority — run `elanus code inbox` to read):",
    );
    for m in &pending {
        let from = m.from.as_deref().unwrap_or("?");
        out.push_str(&format!("\n  From {from}: {}", clip(&m.message, 400)));
    }
    Some(out)
}

/// M4 degradation — when a MID-CYCLE block is visible to a session whose harness
/// CANNOT push mid-cycle (Codex; opencode-headless until the served path lands), it
/// is delivered on the next-turn vector instead. Log that downgrade once per
/// rendering, LEGIBLY (a downgrade, not an error, not a silent drop), so an
/// operator can see "this block wanted mid-cycle, the harness degraded it." Quiet
/// when the harness CAN do mid-cycle (Claude Code) or no block wanted it.
fn log_mid_cycle_degradation(agent_noun: &str, blocks: &[crate::context_store::LoadedBlock]) {
    if achievable_vector(agent_noun, InjectionVector::MidCycle) == InjectionVector::MidCycle {
        return; // the harness can do mid-cycle — nothing to degrade.
    }
    for b in blocks {
        if crate::context_store::is_mid_cycle(b) {
            eprintln!(
                "[code] block {:?} requested mid-cycle delivery but harness {agent_noun:?} \
has no live mid-cycle vector; DEGRADED to next-turn (delivered in this turn's injection)",
                b.name
            );
        }
    }
}

/// The package the agent-comms config lives under (config/packages/agent-comms.toml).
/// C3's high-priority threshold and C4's channel opt-in are read from here so they
/// are owner-tunable (per docs/config.md), not hardcoded.
const COMMS_PACKAGE: &str = "agent-comms";

/// C3 (agent-comms) — the `events.priority` at or above which an UNSEEN inbox
/// message is HIGH-priority and must reach the model mid-cycle (Claude Code), not
/// just next-turn. Read from `agent-comms.high_priority_threshold`; defaults to 5
/// when unset/unparseable (mail priority is 0 by default, so the louder vector is
/// reserved for explicitly-elevated deliveries). Best-effort: any config error
/// falls back to the default rather than breaking the (telemetry-tier) injection.
fn high_priority_threshold(root: &Root) -> i32 {
    const DEFAULT: i32 = 5;
    crate::config_repo::get_key(root, COMMS_PACKAGE, "high_priority_threshold")
        .ok()
        .flatten()
        .and_then(|raw| raw.trim().parse::<i32>().ok())
        .unwrap_or(DEFAULT)
}

/// C4 (agent-comms) — the rooms a profile has OPTED IN to surfacing as
/// `channel:<id>` blocks (`agent-comms.channels`, a TOML array of room ids), and
/// the recent-N bound on each (`agent-comms.channel_recent_n`, default 5). A room
/// is surfaced only when (a) it is in this opt-in list AND (b) the session is
/// actually in that room — the gate is the AND of config + membership, so opting in
/// never widens a session beyond rooms it already belongs to. Empty by default (no
/// channel block at all unless explicitly configured).
fn channel_optin(root: &Root) -> (Vec<String>, usize) {
    let rooms = crate::config_repo::get_key(root, COMMS_PACKAGE, "channels")
        .ok()
        .flatten()
        .and_then(|raw| {
            // The value comes back as a TOML fragment (e.g. `["a","b"]`); parse it
            // directly as a TOML value (toml 1.0 `Value: FromStr` parses one value).
            raw.trim()
                .parse::<toml::Value>()
                .ok()
                .and_then(|v| v.as_array().cloned())
        })
        .map(|arr| {
            arr.into_iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let recent_n = crate::config_repo::get_key(root, COMMS_PACKAGE, "channel_recent_n")
        .ok()
        .flatten()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(5);
    (rooms, recent_n)
}

// NOTED FOLLOW-ON (agent-comms, intentionally NOT built here): the NATIVE-agent
// stage variants. C2/C4 say a native agent surfaces the inbox/channel as a STAGE
// that adds to `doc.system` (rather than the coding-agent per-turn injection). That
// path requires a native agent to HAVE a per-agent inbox/room, which only the
// CODING-AGENT surface has today (`inbox_for_session`, `code_room_members` are all
// `code-*`-session-scoped). When native agents grow a mailbox/room identity, add a
// context stage that calls `inbox_block`/`channel_block` and folds them into
// `doc.system` — the producers above are deliberately harness-agnostic so that
// stage can reuse them. Deferred until the native-agent mailbox exists.

/// C2 (agent-comms) — build the EPHEMERAL `inbox` computed block for a coding
/// session: the unseen-mail count + a preview of the latest, in the same
/// `{name, content}` block shape the durable memory blocks use. Computed each turn
/// from `inbox_for_session` (NOT written to `context_blocks` — the inbox changes
/// every turn). Returns `None` when the inbox has no unseen mail, so a quiet turn
/// produces no block. This is the ONE producer of the inbox surface — the old
/// hardcoded `[elanus] You have N new message(s)` text in `turn_injection` is now
/// just this block's content.
fn inbox_block(unseen: &[codesession::InboxItem]) -> Option<crate::context_store::LoadedBlock> {
    if unseen.is_empty() {
        return None;
    }
    let mut content = format!(
        "You have {} new message(s) in your inbox. Run `elanus code inbox` to read them.",
        unseen.len()
    );
    if let Some(latest) = unseen.last() {
        let from = latest.from.as_deref().unwrap_or("?");
        content.push_str(&format!(
            "\nLatest from {from}: {}",
            clip(&latest.message, 200)
        ));
    }
    Some(crate::context_store::LoadedBlock {
        name: "inbox".to_string(),
        content,
        // Ordering/placement only matter for the render order alongside durable
        // blocks; the inbox is a high-signal status line, so keep it near the top.
        priority: -10,
        placement: crate::context_blocks::Placement::System,
        owner: String::new(),
        scope: crate::context_blocks::Scope::Session,
    })
}

/// C4 (agent-comms) — build the EPHEMERAL `channel:<id>` block for one room the
/// session is in AND the profile opted into: the recent-N shared-channel messages,
/// advisory. `None` when the channel has no recent traffic. Like `inbox_block`,
/// computed each turn, never persisted.
fn channel_block(
    msgs: &[codesession::ChannelMsg],
    room: &str,
) -> Option<crate::context_store::LoadedBlock> {
    if msgs.is_empty() {
        return None;
    }
    let mut content = format!(
        "Recent traffic on shared channel {room} (advisory — what others in this room are saying):"
    );
    for m in msgs {
        let from = m.from.as_deref().unwrap_or("?");
        content.push_str(&format!("\n  {from}: {}", clip(&m.message, 200)));
    }
    Some(crate::context_store::LoadedBlock {
        name: format!("channel:{room}"),
        content,
        priority: 50, // advisory — render after the agent's own blocks
        placement: crate::context_blocks::Placement::System,
        owner: String::new(),
        scope: crate::context_blocks::Scope::Session,
    })
}

pub fn turn_injection(root: &Root, agent_noun: &str, session: &str) -> Option<String> {
    let unseen = codesession::inbox_for_session(root, agent_noun, session, true)
        .ok()
        .unwrap_or_default();
    let note = codesession::get_note(root, session).ok().flatten();

    // M4 (memory-blocks handoff) — the durable memory blocks visible to THIS coding
    // session: its agent-noun-owned agent-scope blocks + its session-scope blocks,
    // ordered by priority. The coding agent has no profile document, so blocks are
    // keyed by agent noun + session (`load_session_blocks`), not a Profile. The
    // `note` block is rendered separately above (as `[elanus note]`) so it is
    // excluded here to avoid showing it twice.
    let blocks = session_memory_blocks(root, agent_noun, session);
    // If any visible block wanted the louder mid-cycle vector but this harness can't
    // push it (Codex; opencode-headless), it is delivered HERE on the next-turn
    // vector — log that legible downgrade rather than dropping or erroring (M4).
    log_mid_cycle_degradation(agent_noun, &blocks);

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

    // C2 (agent-comms) — the inbox is now a COMPUTED block, the one producer of the
    // inbox surface (replacing the old hardcoded `[elanus] You have N message(s)`
    // text). Ephemeral: computed from the unseen mail each turn, never persisted.
    // None when there is no unseen mail, so a quiet inbox adds no block.
    let inbox_blk = inbox_block(&unseen);

    // C4 (agent-comms) — the OPT-IN shared-channel blocks. A room's recent traffic
    // is surfaced as a `channel:<id>` computed block ONLY when the profile opted the
    // room in (config) AND this session is actually in that room. The session's room
    // is its OWN (record/workdir-derived `room` above); opting in a room the session
    // is not in surfaces nothing — the gate is config AND membership.
    let (optin_rooms, channel_recent_n) = channel_optin(root);
    let mut channel_blocks: Vec<crate::context_store::LoadedBlock> = Vec::new();
    if !room.is_empty() && optin_rooms.iter().any(|r| r == &room) {
        if let Ok(msgs) = codesession::room_recent(root, &room, channel_recent_n) {
            if let Some(b) = channel_block(&msgs, &room) {
                channel_blocks.push(b);
            }
        }
    }

    if inbox_blk.is_none()
        && note.is_none()
        && blocks.is_empty()
        && peer_claims.is_empty()
        && live_siblings.is_empty()
        && channel_blocks.is_empty()
    {
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
                // SI1: humanized recency so a viewer can judge alive-vs-stranded
                // before touching a sibling's WIP.
                let since = humanize_since(&s.last_active);
                // SI2: the sibling's CURRENT task, when it has a projected task list.
                // None → render nothing (opencode emits no todo event → honestly
                // absent, never a faked empty list). Quoted + clipped (~80 chars).
                let task = s
                    .current_task
                    .as_ref()
                    .map(|(text, status)| format!(": {status} {:?}", clip_task(text)))
                    .unwrap_or_default();
                // SA2/SA3: the file it last claimed (auto/manual), when known.
                let touching = last_path
                    .get(s.session.as_str())
                    .map(|p| format!(", last editing {}", clip(p, 200)))
                    .unwrap_or_default();
                format!(
                    "{} ({}, last active {since}){task}{touching}",
                    s.session, s.agent_noun
                )
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

    // C2 (agent-comms): the inbox is rendered as its computed block in the same
    // `[elanus block: …]` shape the memory blocks use — one producer, one path. An
    // empty inbox produced no block above, so nothing is emitted here (the quiet
    // turn is preserved). The old hardcoded "[elanus] You have N message(s)" text is
    // gone; its content moved into `inbox_block`.
    if let Some(b) = &inbox_blk {
        out.push_str(&format!(
            "[elanus block: {}] {}",
            b.name,
            clip(&b.content, 2000)
        ));
    }
    if let Some(note) = note {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("[elanus note] {}", clip(&note, 2000)));
    }
    // M4: render the session's memory blocks, in priority order, reusing the
    // built-in {name, text} block shape. One labeled line per block so the agent
    // reads each as a distinct, named chunk of durable context.
    for b in &blocks {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "[elanus block: {}] {}",
            b.name,
            clip(&b.content, 2000)
        ));
    }
    // C4 (agent-comms): the opt-in shared-channel blocks, same block shape. Advisory.
    for b in &channel_blocks {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "[elanus block: {}] {}",
            b.name,
            clip(&b.content, 2000)
        ));
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

fn launch_external_harness(
    root: &Root,
    external: ExternalHarness,
    args: &[String],
    headless: bool,
    want_brief: bool,
    requested_grants: codesession::RequestedGrants,
    parent: Option<String>,
    room: Option<String>,
    model: Option<&str>,
    effort: Option<&str>,
    provider: Option<&str>,
    skills: &[(String, PathBuf)],
) -> Result<()> {
    let session = launch_session_id(root);
    let principal = session.clone();
    let agent = external.decl.agent_noun.clone();
    let mode = if headless { Mode::Headless } else { Mode::Tui };
    let mode_str = match mode {
        Mode::Tui => "tui",
        Mode::Headless => "headless",
    };
    let workdir = std::env::current_dir().unwrap_or_else(|_| root.dir.clone());
    let adapter = external.package_dir.join(&external.decl.run);
    if !adapter.exists() {
        bail!(
            "external harness {:?} from package {:?} points at missing adapter {}",
            external.decl.name,
            external.package,
            adapter.display()
        );
    }

    let token = codesession::mint(
        root,
        &principal,
        &agent,
        std::process::id() as i32,
        parent.as_deref(),
        requested_grants,
    )
    .with_context(|| format!("minting the session credential for {principal}"))?;
    let bus_token = token.secret.clone();

    let scratch = root.run_dir().join(&session);
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("creating run scratch {}", scratch.display()))?;
    let skills_dir = scratch.join("skills");
    let summary_path = scratch.join("adapter-summary.json");

    let prompt = (!args.is_empty()).then(|| args.join(" "));
    let brief_text = want_brief.then(|| briefing(&session));
    // The workdir-derived coordination room (SA1) — same as a built-in session, so the
    // external adapter's auto-claims land where siblings see them.
    let room = resolve_room(room.as_deref(), &workdir);

    // Setup + exec inside the closure so the scratch/token cleanup below ALWAYS runs,
    // even if a setup step (skill link, record, room join) fails.
    let status_result = (|| -> Result<std::process::ExitStatus> {
        if !skills.is_empty() {
            link_skill_packages(&skills_dir, skills)
                .with_context(|| format!("linking skills into {}", skills_dir.display()))?;
        }
        codesession::upsert_record(
            root,
            &codesession::SessionRecord {
                elanus_session: session.clone(),
                native_session: session.clone(),
                tool: external.decl.name.clone(),
                agent_noun: agent.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
                room: None,
            },
        )
        .with_context(|| format!("recording external harness session {session}"))?;
        // Join the coordination room so this session's advisory claims are REAPED when
        // it dies (reap_dead_members keys on code_room_members) — parity with built-ins.
        let _ = codesession::set_room(root, &session, &room);
        let _ = codesession::join_room(
            root,
            &room,
            &session,
            &agent,
            std::process::id() as i32,
        );

        // Emit the same launch envelope the direct path used to own.
        publish_obs(
            root,
            &principal,
            &bus_token,
            &obs_topic(&agent, &session, "session/start"),
            json!({
                "ts": now_iso(),
                "tool": &external.decl.name,
                "workdir": workdir.display().to_string(),
                "args": args,
                "parent": parent,
                "model": model,
                "effort": effort,
                "provider": provider,
            }),
        );

        let mut cmd = std::process::Command::new(&adapter);
        scrub_launch_control_env(&mut cmd);
        cmd.current_dir(&workdir)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .env("ELANUS_ROOT", &root.dir)
            .env(ENV_SESSION, &session)
            .env(ENV_AGENT, &agent)
            .env("ELANUS_PACKAGE", &principal)
            .env(crate::harness::ENV_BUS_TOKEN, &bus_token)
            .env(crate::harness::ENV_WORKDIR, &workdir)
            .env(crate::harness::ENV_MODE, mode_str)
            .env(crate::harness::ENV_TOOL, &external.decl.name);
        if let Some(model) = model {
            cmd.env(crate::harness::ENV_MODEL, model);
        }
        if let Some(provider) = provider {
            cmd.env(crate::harness::ENV_PROVIDER, provider);
        }
        cmd.env(crate::harness::ENV_SUMMARY_FILE, &summary_path);
        // The FULL raw argv (harness flags + prompt) — real adapters split it via
        // their capture fn. The joined ENV_PROMPT is kept for simple adapters.
        cmd.env(
            crate::harness::ENV_ARGS,
            serde_json::to_string(args).unwrap_or_else(|_| "[]".into()),
        );
        if let Some(prompt) = &prompt {
            cmd.env(crate::harness::ENV_PROMPT, prompt);
        }
        if let Some(brief) = &brief_text {
            cmd.env(crate::harness::ENV_BRIEFING, brief);
        }
        if !skills.is_empty() {
            cmd.env(crate::harness::ENV_SKILLS_DIR, &skills_dir);
        }
        eprintln!(
            "[code] launching external harness {} from package {} as session {session}",
            external.decl.name, external.package
        );
        let mut child = cmd
            .spawn()
            .with_context(|| format!("launching external adapter {}", adapter.display()))?;
        child
            .wait()
            .with_context(|| format!("waiting for external adapter {}", adapter.display()))
    })();

    let summary = read_capture_summary_file(Some(&summary_path)).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&scratch);
    let exit_code = status_result.as_ref().ok().and_then(|status| status.code());
    publish_obs(
        root,
        &principal,
        &bus_token,
        &obs_topic(&agent, &session, "session/stop"),
        json!({ "ts": now_iso(), "exit_code": exit_code }),
    );

    if let Some(reply_to) = std::env::var(ENV_REPLY_TO).ok().filter(|s| !s.is_empty()) {
        let correlation = std::env::var(ENV_REPLY_CORRELATION)
            .ok()
            .filter(|s| !s.is_empty());
        let launch_error = status_result.as_ref().err().map(|e| format!("{e:#}"));
        let fallback_summary;
        let (status, summary) = match status_result.as_ref() {
            Ok(status) => (Some(status), &summary),
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

    codesession::retire(root, &principal);

    let status = status_result?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

/// `elanus code <tool> [args...]` — launch the real coding agent, observed.
pub fn launch(root: &Root, tool: &str, args: &[String], provider: Option<&str>) -> Result<()> {
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
    // worker shape rides `--worker`. M4 grant-narrowing flags (`--budget`,
    // `--grant-*`) are also elanus-only and stripped before the tool sees argv.
    // Order matters: pull M4 flags first (they may appear anywhere), then the
    // others; all return filtered args.
    let (requested_grants, args) = take_grants_flags(args)?;
    let (want_brief, args) = take_brief_flag(&args);
    let (room, args) = take_room_flag(&args);
    let (headless, args) = take_headless_flag(&args);
    let (profile_name, args) = take_profile_flag(&args);
    let args = &args[..];
    let (model, effort) = extract_model_effort(args);

    // The profile's visible skills, materialized into the harness's per-session
    // skills dir below (Claude `--plugin-dir` plugin / Codex `$CODEX_HOME` /
    // opencode `$OPENCODE_CONFIG_DIR`). Computed once here; `--profile` selects the
    // profile (default "default"). Empty (best-effort) when the profile has no
    // visible skills or discovery fails — then a session sees only the bootstrap
    // `/elanus` skill, exactly as before.
    let skills = visible_skill_packages(root, &profile_name);
    let external = resolve_external_harness(root, &profile_name, tool)?;
    return launch_external_harness(
        root,
        external,
        args,
        headless,
        want_brief,
        requested_grants,
        parent,
        room,
        model.as_deref(),
        effort.as_deref(),
        provider,
        &skills,
    );
}

struct ClaudeLaunch<'a> {
    root: &'a Root,
    principal: &'a str,
    bus_token: &'a str,
    agent: &'a str,
    session: &'a str,
    workdir: &'a Path,
    args: &'a [String],
    brief: Option<&'a str>,
    worker: bool,
    worker_timeout: Option<u64>,
    injection: Option<&'a crate::provider::HarnessInjection>,
    skills: &'a [(String, PathBuf)],
}

/// Reconstruct the launch argv that the thin adapter binaries pass to a capture
/// function from `ELANUS_CODE_PROMPT`. The prompt is the user's TASK and must travel
/// as ONE positional arg — splitting on whitespace shreds a multi-word prompt (codex
/// then keeps only the first token). So the whole prompt is a single argv element.
fn adapter_prompt_args(prompt: Option<&str>) -> Vec<String> {
    match prompt {
        Some(p) if !p.is_empty() => vec![p.to_string()],
        _ => Vec::new(),
    }
}

/// The argv an adapter passes to its capture fn: the FULL raw argv (harness flags +
/// prompt) from `ELANUS_CODE_ARGS` so codex `-c …`/etc. reach the tool and the prompt
/// is parsed correctly — falling back to the single joined prompt when no argv was set.
fn adapter_args(ctx: &crate::harness::Ctx) -> Vec<String> {
    if ctx.args().is_empty() {
        adapter_prompt_args(ctx.prompt())
    } else {
        ctx.args().to_vec()
    }
}

/// Reconstruct the visible skill package list from `ELANUS_CODE_SKILLS_DIR`.
fn skill_packages_from_dir(skills_dir: Option<&Path>) -> Vec<(String, PathBuf)> {
    let Some(skills_dir) = skills_dir else {
        return Vec::new();
    };
    let read_dir = match std::fs::read_dir(skills_dir) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!(
                "[code] reading skills dir {} for adapter launch failed: {e:#}",
                skills_dir.display()
            );
            return Vec::new();
        }
    };
    let mut skills: Vec<(String, PathBuf)> = read_dir
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let target = std::fs::canonicalize(entry.path()).unwrap_or_else(|_| entry.path());
            Some((name, target))
        })
        .collect();
    skills.sort_by(|a, b| a.0.cmp(&b.0));
    skills
}

/// Extracted Claude hook-bridge capture path, shared by `launch()` and the thin
/// adapter binary entrypoint.
#[allow(clippy::too_many_arguments)]
fn run_claude_capture(ctx: ClaudeLaunch<'_>) -> Result<(std::process::ExitStatus, CaptureSummary)> {
    use std::process::Command;

    let ClaudeLaunch {
        root,
        principal,
        bus_token,
        agent,
        session,
        workdir: _workdir,
        args,
        brief,
        worker,
        worker_timeout,
        injection,
        skills,
    } = ctx;
    let scratch = root.run_dir().join(session);
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("creating run scratch {}", scratch.display()))?;
    let settings_path = scratch.join("settings.json");
    let self_exe = elanus_command_path()?;
    let binary = "claude";
    let result = (|| -> Result<(std::process::ExitStatus, CaptureSummary)> {
        let settings = claude_settings(&self_exe, root);
        std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)
            .with_context(|| format!("writing {}", settings_path.display()))?;
        // M1: the bootstrap `/elanus` skill + the profile's visible skills,
        // delivered as a per-session plugin (the only channel that surfaces
        // skills under `--setting-sources ''` from an ephemeral path).
        let plugin_dir = build_claude_skill_plugin(&scratch, skills)?;

        // Launch the real binary with the generated, isolated config. The
        // TUI gets inherited stdio so it is a normal, fully usable session.
        // `--setting-sources ''` loads NO user/project/local settings (the
        // user's ~/.claude hooks/CLAUDE.md are untouched); `--settings <file>`
        // loads only our generated hooks (Appendix A).
        if worker {
            let mut tool_args = vec![
                "--settings".to_string(),
                settings_path.display().to_string(),
                "--setting-sources".to_string(),
                "".to_string(),
                "--plugin-dir".to_string(),
                plugin_dir.display().to_string(),
            ];
            if let Some(brief) = &brief {
                tool_args.push("--append-system-prompt".to_string());
                tool_args.push(brief.to_string());
            }
            tool_args.push("-p".to_string());
            tool_args.extend_from_slice(args);
            let timeout_suffix;
            let (program, tool_args) = if let Some(secs) = worker_timeout {
                timeout_suffix = format!(" [timeout {secs}s]");
                timeout_wrap(binary, &tool_args, secs)
            } else {
                timeout_suffix = String::new();
                (binary.to_string(), tool_args)
            };
            let mut cmd = Command::new(&program);
            cmd.args(&tool_args);
            // Scrub elanus's provider credentials FIRST so Claude Code uses
            // its own login (`~/.claude`) rather than inheriting elanus's
            // DeepSeek ANTHROPIC_BASE_URL/API_KEY (Task 2). The ELANUS_*
            // vars set below are NOT scrubbed — the hook bridge depends on
            // them.
            scrub_provider_creds(&mut cmd);
            scrub_launch_control_env(&mut cmd);
            // M2: if `--provider` was given, apply the materialized injection
            // AFTER the scrub (claude: ANTHROPIC_BASE_URL + ANTHROPIC_AUTH_TOKEN,
            // overriding the Claude.AI login). No-op when absent.
            if let Some(inj) = injection {
                apply_provider_injection_env(&mut cmd, inj);
            }
            // The session's own identity, carried to the hook bridge children
            // CC spawns. ELANUS_PACKAGE + ELANUS_BUS_TOKEN are what
            // `elanus bus pub` authenticates with (src/buscli.rs); ELANUS_CODE_*
            // tell the bridge which session/agent to file under.
            cmd.env("ELANUS_PACKAGE", principal)
                .env("ELANUS_BUS_TOKEN", bus_token)
                .env(ENV_SESSION, session)
                .env(ENV_AGENT, agent)
                .env("ELANUS_ROOT", &root.dir);
            eprintln!(
                "[code] launching {} as session {session}{timeout_suffix}",
                binary
            );
            cmd.stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::inherit());
            let output = cmd.output().with_context(|| {
                format!("launching {program} (is it installed and on PATH?)")
            })?;
            let text = String::from_utf8_lossy(&output.stdout);
            print_claude_worker_result(session, &text);
            let final_text = (!text.trim().is_empty()).then(|| clip(&text, FINAL_TEXT_CAP));
            Ok((
                output.status,
                CaptureSummary {
                    final_text,
                    file_changes: Vec::new(),
                },
            ))
        } else {
            // Foreground/interactive launches are deliberately NOT wrapped in
            // timeout; real live delegations can run as long as needed.
            let mut cmd = Command::new(binary);
            cmd.arg("--settings")
                .arg(&settings_path)
                .arg("--setting-sources")
                .arg("")
                .arg("--plugin-dir")
                .arg(&plugin_dir);
            // The launch-envelope briefing (M4-B): Claude Code injects it
            // out-of-band via --append-system-prompt (the system layer, after
            // the cached prefix — Appendix A), not the user message.
            if let Some(brief) = &brief {
                cmd.arg("--append-system-prompt").arg(brief);
            }
            // Scrub elanus's provider credentials FIRST so Claude Code uses
            // its own login (`~/.claude`) rather than inheriting elanus's
            // DeepSeek ANTHROPIC_BASE_URL/API_KEY (Task 2). The ELANUS_*
            // vars set below are NOT scrubbed — the hook bridge depends on
            // them.
            scrub_provider_creds(&mut cmd);
            scrub_launch_control_env(&mut cmd);
            // M2: apply the named-provider injection (if any) after the scrub.
            if let Some(inj) = injection {
                apply_provider_injection_env(&mut cmd, inj);
            }
            // The session's own identity, carried to the hook bridge children
            // CC spawns. ELANUS_PACKAGE + ELANUS_BUS_TOKEN are what
            // `elanus bus pub` authenticates with (src/buscli.rs); ELANUS_CODE_*
            // tell the bridge which session/agent to file under.
            cmd.env("ELANUS_PACKAGE", principal)
                .env("ELANUS_BUS_TOKEN", bus_token)
                .env(ENV_SESSION, session)
                .env(ENV_AGENT, agent)
                .env("ELANUS_ROOT", &root.dir);
            eprintln!("[code] launching {} as session {session}", binary);
            cmd.args(args);
            let status = cmd.status().with_context(|| {
                format!("launching {} (is it installed and on PATH?)", binary)
            })?;
            Ok((status, CaptureSummary::default()))
        }
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// Shared adapter prompt reconstruction and capture delegation. The adapter
/// binaries are thin wrappers around these entrypoints.
fn adapter_provider_injection(
    ctx: &crate::harness::Ctx,
) -> Result<Option<crate::provider::HarnessInjection>> {
    let Some(name) = ctx.provider() else {
        return Ok(None);
    };
    let conn = crate::db::open(ctx.root())
        .with_context(|| "opening the ledger to resolve --provider".to_string())?;
    let prov = crate::provider::get(ctx.root(), &conn, name)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no provider named {name:?} — define one with `elanus provider add {name} …` \
             (list with `elanus provider list`)"
        )
    })?;
    let hid = crate::provider::HarnessId::parse(ctx.tool())?;
    match crate::provider::materialize(
        name,
        &prov.credential,
        crate::provider::Consumer::Harness(hid),
        ctx.model(),
    )? {
        crate::provider::Injection::Harness(h) => Ok(Some(h)),
        crate::provider::Injection::Dispatcher(_) => unreachable!(
            "materialize(Consumer::Harness) always returns Injection::Harness"
        ),
    }
}

pub fn run_claude_adapter(ctx: &crate::harness::Ctx) -> Result<std::process::ExitStatus> {
    let bus_token = ctx.bus_token().context("missing ELANUS_BUS_TOKEN")?;
    let skills = skill_packages_from_dir(ctx.skills_dir());
    let args = adapter_args(ctx);
    let injection = adapter_provider_injection(ctx)?;
    let (status, summary) = run_claude_capture(ClaudeLaunch {
        root: ctx.root(),
        principal: ctx.session(),
        bus_token,
        agent: ctx.agent_noun(),
        session: ctx.session(),
        workdir: ctx.workdir(),
        args: &args,
        brief: ctx.briefing(),
        worker: ctx.mode() == Mode::Headless,
        worker_timeout: None,
        injection: injection.as_ref(),
        skills: &skills,
    })?;
    write_capture_summary_file(ctx.summary_file(), &summary);
    Ok(status)
}

pub fn run_codex_adapter(ctx: &crate::harness::Ctx) -> Result<std::process::ExitStatus> {
    let bus_token = ctx.bus_token().context("missing ELANUS_BUS_TOKEN")?;
    let skills = skill_packages_from_dir(ctx.skills_dir());
    let args = adapter_args(ctx);
    let injection = adapter_provider_injection(ctx)?;
    let (status, summary) = match ctx.mode() {
        Mode::Headless => run_codex_capture(
            ctx.root(),
            ctx.session(),
            bus_token,
            ctx.agent_noun(),
            ctx.session(),
            ctx.workdir(),
            &args,
            ctx.briefing(),
            None,
            injection.as_ref(),
            &skills,
        )?,
        Mode::Tui => run_codex_tui_import(
            ctx.root(),
            ctx.session(),
            bus_token,
            ctx.agent_noun(),
            ctx.session(),
            ctx.workdir(),
            &args,
            ctx.briefing(),
            injection.as_ref(),
            &skills,
        )?,
    };
    write_capture_summary_file(ctx.summary_file(), &summary);
    Ok(status)
}

pub fn run_opencode_adapter(ctx: &crate::harness::Ctx) -> Result<std::process::ExitStatus> {
    let bus_token = ctx.bus_token().context("missing ELANUS_BUS_TOKEN")?;
    let skills = skill_packages_from_dir(ctx.skills_dir());
    let args = adapter_args(ctx);
    let injection = adapter_provider_injection(ctx)?;
    let (status, summary) = match ctx.mode() {
        Mode::Headless => run_opencode_capture(
            ctx.root(),
            ctx.session(),
            bus_token,
            ctx.agent_noun(),
            ctx.session(),
            ctx.workdir(),
            &args,
            ctx.briefing(),
            true,
            None,
            injection.as_ref(),
            &skills,
        )?,
        Mode::Tui => run_opencode_tui_server_events(
            ctx.root(),
            ctx.session(),
            bus_token,
            ctx.agent_noun(),
            ctx.session(),
            ctx.workdir(),
            &args,
            ctx.briefing(),
            injection.as_ref(),
            &skills,
        )?,
    };
    write_capture_summary_file(ctx.summary_file(), &summary);
    Ok(status)
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
    injection: Option<&crate::provider::HarnessInjection>,
    skills: &[(String, PathBuf)],
) -> Result<(std::process::ExitStatus, CaptureSummary)> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let args = codex_args_with_prompt_from_stdin(args)?;
    let prompt_env = split_codex_seed_prompt(&args).1;

    // Point codex at an isolated per-session home carrying the generated hook config
    // (always) and profile skills (when present), while symlinking auth so the user's
    // native codex login survives the redirect.
    let codex_home = build_codex_skills_home(root, session, skills)?;

    let mut codex_args = vec![
        "exec".to_string(),
        "--json".to_string(),
        "--skip-git-repo-check".to_string(),
        "--dangerously-bypass-hook-trust".to_string(),
    ];
    // M2: a `--provider` codex injection is a set of `-c model_provider…` flags that
    // must ride with the `codex exec` options, BEFORE the prompt positional. The
    // secret rides the env (env_key), never the command line.
    if let Some(inj) = injection {
        codex_args.extend(inj.args.iter().cloned());
    }
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
    // (the human sees Codex's own output). CODEX_HOME is set below to the session
    // home, which symlinks the user's real auth.
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
    // M2: apply the named-provider injection (if any) after the scrub. For codex
    // this carries the env_key secret + removes inherited CODEX_* so a parent's
    // provider can't bleed into this child.
    if let Some(inj) = injection {
        apply_provider_injection_env(&mut cmd, inj);
    }
    cmd.env_remove("ELANUS_PACKAGE")
        .env_remove("ELANUS_BUS_TOKEN");
    // The session's own identity, carried to anything the codex session spawns —
    // crucially `elanus code deliver`, which reads ELANUS_CODE_SESSION/AGENT to
    // record the running session as the requester, and ELANUS_ROOT to resolve the
    // same root. ELANUS_BUS_TOKEN stays SCRUBBED (above): the codex child is the
    // tool, not an elanus adapter — its hook claims via the ledger (no bus), so it
    // needs no bus credential. The bus token enters the launch contract only for an
    // actual adapter process (PH3), never the raw tool child.
    cmd.env(ENV_SESSION, session)
        .env(ENV_AGENT, agent)
        .env("ELANUS_ROOT", &root.dir)
        .env(crate::harness::ENV_WORKDIR, workdir)
        .env(crate::harness::ENV_MODE, "headless")
        .env(crate::harness::ENV_TOOL, "codex");
    if let Some(brief) = brief {
        cmd.env(crate::harness::ENV_BRIEFING, brief);
    }
    if let Some(prompt) = prompt_env.as_deref() {
        cmd.env(crate::harness::ENV_PROMPT, prompt);
    }
    if !skills.is_empty() {
        cmd.env(crate::harness::ENV_SKILLS_DIR, codex_home.join("skills"));
    }
    // Point codex at the per-session home (set LAST so it wins over any
    // inherited/injection CODEX_HOME).
    cmd.env("CODEX_HOME", &codex_home);
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
    print_stream_worker_result(agent, session, &summary);

    let status = child.wait().context("waiting for codex exec to finish")?;
    Ok((status, summary))
}

// ── HM2: codex TUI (RolloutImport) ────────────────────────────────────────────
//
// The codex TUI cell. Its interactive TUI prints nothing parseable to stdout, so
// the faithful way to capture obs for an interactive codex session is to read the
// rollout JSONL the TUI writes to
// `~/.codex/sessions/<Y>/<M>/<D>/rollout-<ts>-<thread_id>.jsonl` AFTER it exits.
// Live apply_patch edit-claims ride the generated PostToolUse hook.
//
// WHY rollout-import for obs rather than hooks/JSON: forcing codex's headless
// `--json` event stream onto an interactive TUI is fragile (couples to
// undocumented flag interplay). The rollout is codex's OWN first-class,
// documented on-disk record of the exact turns; it survives a launcher crash.
// The cost — it is post-hoc and coarser than a live hook bridge — is declared
// honestly: every projected event is marked `fidelity=rollout-import` (see
// `rollout_map_record`). The generated hook below is narrower: live edit-claims.
//
// !!! FLAGGED GAP (accepted, NOT hidden) !!!
// This path is built + unit-tested against the REAL on-disk rollout schema (a
// trimmed fixture captured from `~/.codex/sessions`, `ROLLOUT_FIXTURE` below), but
// the LIVE codex TUI launch + import of a FRESH rollout is UNVERIFIED end-to-end:
// running a real interactive codex TUI needs codex credits + a TTY, which this
// environment does not have. The reader is exercised by the fixture test; the
// command construction is exercised by the mode/arg tests; the live round trip is
// the only unproven link and is gated on codex credits.
//
// SCOPE: HM2 + HM3 only. HM4 (the briefing-prose rewrite + doc sweep) and HM5 (the
// adapter checklist doc) are a SEPARATE task and are deliberately NOT done here.
#[allow(clippy::too_many_arguments)]
fn run_codex_tui_import(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    workdir: &Path,
    args: &[String],
    brief: Option<&str>,
    injection: Option<&crate::provider::HarnessInjection>,
    skills: &[(String, PathBuf)],
) -> Result<(std::process::ExitStatus, CaptureSummary)> {
    use std::process::Command;

    // Build the interactive `codex` invocation. Bare `codex` (no prompt) opens the
    // TUI; `codex "<prompt>"` opens the TUI seeded with that prompt. Codex has no
    // --append-system-prompt, so the launch-envelope briefing is PREPENDED to the
    // seed prompt positional (codex's documented seed channel): we fold the brief
    // and the user's seed prompt (if any) into one positional. With no user prompt
    // AND no brief, we pass nothing → a bare TUI.
    let mut codex_args: Vec<String> = vec!["--dangerously-bypass-hook-trust".to_string()];
    // M2: a `--provider` codex injection rides as `-c model_provider…` flags before
    // the seed positional (same shape as the exec path, just the TUI cell).
    if let Some(inj) = injection {
        codex_args.extend(inj.args.iter().cloned());
    }
    // Pass through any non-prompt flags the user supplied (e.g. -m/--model) before
    // the seed positional; the prompt itself, if present, is the LAST positional.
    let (flags, user_prompt) = split_codex_seed_prompt(args);
    codex_args.extend(flags);
    let seed = match (brief, user_prompt.as_deref()) {
        (Some(b), Some(p)) => Some(format!("{}\n\n{}", codex_briefing_block(b), p)),
        (Some(b), None) => Some(codex_briefing_block(b)),
        (None, Some(p)) => Some(p.to_string()),
        (None, None) => None,
    };
    if let Some(seed) = seed {
        codex_args.push(seed);
    }

    let mut cmd = Command::new("codex");
    cmd.args(&codex_args);
    cmd.current_dir(workdir);
    // The TUI is a real, human-pumped interactive session: inherit ALL of stdio so
    // it is fully usable (exactly like Claude's TUI). The launcher parses NOTHING
    // live — capture is the post-hoc rollout import below.
    cmd.stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    scrub_provider_creds(&mut cmd);
    scrub_launch_control_env(&mut cmd);
    // M2: apply the named-provider injection (if any) after the scrub.
    if let Some(inj) = injection {
        apply_provider_injection_env(&mut cmd, inj);
    }
    cmd.env_remove("ELANUS_PACKAGE")
        .env_remove("ELANUS_BUS_TOKEN");
    // ELANUS_BUS_TOKEN stays scrubbed: the codex child is the tool, not an adapter;
    // its hook claims via the ledger (no bus). (See the exec path for the rationale.)
    cmd.env(ENV_SESSION, session)
        .env(ENV_AGENT, agent)
        .env("ELANUS_ROOT", &root.dir)
        .env(crate::harness::ENV_WORKDIR, workdir)
        .env(crate::harness::ENV_MODE, "tui")
        .env(crate::harness::ENV_TOOL, "codex");
    if let Some(brief) = brief {
        cmd.env(crate::harness::ENV_BRIEFING, brief);
    }
    if let Some(prompt) = user_prompt.as_deref() {
        cmd.env(crate::harness::ENV_PROMPT, prompt);
    }
    // The per-session CODEX_HOME carries generated hooks (always) and profile skills
    // (when present). Set LAST so it wins over inherited/injected CODEX_HOME.
    let codex_home = build_codex_skills_home(root, session, skills)?;
    if !skills.is_empty() {
        cmd.env(crate::harness::ENV_SKILLS_DIR, codex_home.join("skills"));
    }
    cmd.env("CODEX_HOME", &codex_home);
    eprintln!("[code] launching codex TUI as session {session} (rollout import on exit)");

    // Remember the newest rollout that already exists, so after the TUI exits we
    // can pick the rollout it CREATED (a strictly newer mtime) rather than an old
    // one from a prior session. Best-effort: if we can't read the dir, the
    // post-exit resolver simply takes the global newest match.
    let before = newest_rollout_mtime();

    let status = cmd
        .status()
        .with_context(|| "launching codex (is it installed and on PATH?)".to_string())?;

    // Post-hoc import: resolve the rollout the TUI just wrote and project it. We
    // find the freshest rollout created during this launch by the mtime watermark
    // (at launch we don't yet know codex's thread id — it's INSIDE the rollout),
    // then re-resolve by the thread id in its session_meta via the handoff-specified
    // `rollout-*-<thread_id>.jsonl` resolver so the authoritative selection keys on
    // the native thread id (and a same-second sibling can't shadow it).
    let resolved = resolve_codex_rollout(before).map(|path| {
        rollout_thread_id(&path)
            .and_then(|tid| resolve_codex_rollout_by_thread(&tid))
            .unwrap_or(path)
    });
    let summary = match resolved {
        Some(path) => {
            eprintln!("[code] importing codex rollout {}", path.display());
            import_codex_rollout(
                root,
                principal,
                bus_token,
                agent,
                session,
                &path,
                Some(workdir),
            )
        }
        None => {
            eprintln!(
                "[code] no codex rollout found to import for session {session} \
                 (the TUI may have written none); recorded only the lifecycle brackets"
            );
            CaptureSummary::default()
        }
    };
    Ok((status, summary))
}

/// Split a codex launch argv into (pass-through flags, the user's seed prompt).
/// Mirrors `codex_args_have_prompt`'s conservative flag model: skip values for the
/// common value-taking flags, honor `--`, and treat the first remaining non-flag
/// token as the seed prompt (everything else stays as flags). Used to fold the
/// briefing into the seed positional for the TUI without a full codex clap parse.
fn split_codex_seed_prompt(args: &[String]) -> (Vec<String>, Option<String>) {
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
    let mut flags = Vec::new();
    let mut prompt = None;
    let mut after_dash_dash = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if after_dash_dash {
            // Everything after `--` is the prompt (join with spaces if several).
            let rest = args[i..].join(" ");
            prompt = Some(rest);
            break;
        }
        if arg == "--" {
            after_dash_dash = true;
            i += 1;
            continue;
        }
        if value_flags.iter().any(|f| arg == f) {
            flags.push(arg.clone());
            if let Some(v) = args.get(i + 1) {
                flags.push(v.clone());
            }
            i += 2;
            continue;
        }
        if arg.starts_with('-') {
            flags.push(arg.clone());
            i += 1;
            continue;
        }
        // First non-flag token: the seed prompt.
        prompt = Some(arg.clone());
        break;
    }
    (flags, prompt)
}

/// The codex sessions root (`~/.codex/sessions`). Honors `CODEX_HOME` (codex's own
/// override) and falls back to `$HOME/.codex`. Returns None if neither is set.
fn codex_sessions_dir() -> Option<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))?;
    Some(home.join("sessions"))
}

/// The mtime of the newest `rollout-*.jsonl` anywhere under the codex sessions
/// dir, or None if there is none / the dir is unreadable. Used as a watermark so
/// the post-exit resolver can prefer a rollout created DURING this launch.
fn newest_rollout_mtime() -> Option<std::time::SystemTime> {
    let dir = codex_sessions_dir()?;
    walk_rollouts(&dir)
        .into_iter()
        .filter_map(|p| std::fs::metadata(&p).and_then(|m| m.modified()).ok())
        .max()
}

/// Resolve the rollout file the just-exited codex TUI wrote. Prefer the newest
/// rollout strictly newer than `watermark` (the one this launch created); if none
/// is strictly newer (clock granularity, or the only rollout IS this one), fall
/// back to the global newest. None if there are no rollouts at all.
fn resolve_codex_rollout(watermark: Option<std::time::SystemTime>) -> Option<PathBuf> {
    let dir = codex_sessions_dir()?;
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = walk_rollouts(&dir)
        .into_iter()
        .filter_map(|p| {
            let m = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            Some((m, p))
        })
        .collect();
    candidates.sort_by_key(|(m, _)| *m);
    if let Some(wm) = watermark {
        if let Some((_, p)) = candidates.iter().rev().find(|(m, _)| *m > wm) {
            return Some(p.clone());
        }
    }
    candidates.pop().map(|(_, p)| p)
}

/// Resolve a rollout file by its native thread id (the UUID in the filename),
/// globbing `rollout-*-<thread_id>.jsonl` under the codex sessions dir and picking
/// the newest match — the handoff-specified resolver. Used by the TUI import path
/// to confirm/select the rollout once the recorded `native_session` (codex thread
/// id) is known, and available to any future re-import (resume) caller. None if no
/// file matches.
fn resolve_codex_rollout_by_thread(thread_id: &str) -> Option<PathBuf> {
    let dir = codex_sessions_dir()?;
    find_rollout_by_thread(&dir, thread_id)
}

/// Pure thread-id rollout resolver over an explicit sessions dir (unit-testable):
/// the newest `rollout-*-<thread_id>.jsonl` under `dir`, or None.
fn find_rollout_by_thread(dir: &Path, thread_id: &str) -> Option<PathBuf> {
    let needle = format!("-{thread_id}.jsonl");
    let mut matches: Vec<(std::time::SystemTime, PathBuf)> = walk_rollouts(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&needle))
        })
        .filter_map(|p| {
            let m = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            Some((m, p))
        })
        .collect();
    matches.sort_by_key(|(m, _)| *m);
    matches.pop().map(|(_, p)| p)
}

/// Read codex's native thread id from a rollout file: the `session_meta`'s
/// `payload.id` on the first line. None if the file is unreadable or has no
/// session_meta header. (The id is also the UUID in the filename, but reading the
/// header is authoritative and tolerates a renamed file.)
fn rollout_thread_id(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let first = text.lines().find(|l| !l.trim().is_empty())?;
    let rec: Value = serde_json::from_str(first).ok()?;
    if rec.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    rec.get("payload")
        .and_then(|p| p.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Recursively collect every `rollout-*.jsonl` under `dir` (codex nests by
/// `<Y>/<M>/<D>/`). Bounded, best-effort; an unreadable subdir is skipped.
fn walk_rollouts(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                    out.push(path);
                }
            }
        }
    }
    out
}

/// Read a codex TUI rollout JSONL and project its turns into the obs grammar under
/// the elanus session, publishing each as the session principal — the POST-HOC
/// counterpart to `capture_codex_stream`'s live parse. Every projected body is
/// marked `fidelity=rollout-import` so a consumer never mistakes it for live
/// granularity. When `record_workdir` is Some, the `session_meta`'s codex thread id
/// persists the durable `code_sessions` record so the imported TUI session is
/// resumable. Returns the legible summary (final agent text + changed paths).
fn import_codex_rollout(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    rollout_path: &Path,
    record_workdir: Option<&Path>,
) -> CaptureSummary {
    let text = match std::fs::read_to_string(rollout_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "[code] reading codex rollout {} failed (continuing): {e:#}",
                rollout_path.display()
            );
            return CaptureSummary::default();
        }
    };
    project_codex_rollout(
        root,
        principal,
        bus_token,
        agent,
        session,
        &text,
        record_workdir,
    )
}

/// The pure projection core (separated from file IO so it is unit-testable against
/// a fixture string). Parses each JSONL record, persists the durable record from
/// `session_meta`, maps each record to an obs leaf+body via `rollout_map_record`,
/// and harvests the legible summary. A malformed line is skipped (never dropped
/// silently to obs — a parse error is just not a record).
fn project_codex_rollout(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    rollout: &str,
    record_workdir: Option<&Path>,
) -> CaptureSummary {
    let mut summary = CaptureSummary::default();
    for line in rollout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Persist the durable session record from session_meta (the thread id is
        // the rollout's `payload.id`), so an imported TUI session is resumable.
        if let Some(workdir) = record_workdir {
            if rec.get("type").and_then(Value::as_str) == Some("session_meta") {
                if let Some(thread_id) = rec
                    .get("payload")
                    .and_then(|p| p.get("id"))
                    .and_then(Value::as_str)
                {
                    let srec = codesession::SessionRecord {
                        elanus_session: session.to_string(),
                        native_session: thread_id.to_string(),
                        tool: "codex".to_string(),
                        agent_noun: agent.to_string(),
                        workdir: workdir.display().to_string(),
                        room: None,
                    };
                    if let Err(e) = codesession::upsert_record(root, &srec) {
                        eprintln!("[code] recording imported codex session (continuing): {e:#}");
                    }
                }
            }
        }
        rollout_collect_summary(&rec, &mut summary);
        if let Some((leaf, body)) = rollout_map_record(&rec) {
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

/// The post-hoc import provenance marker stamped on EVERY projected rollout body.
/// `fidelity=rollout-import` (NOT live) + `source=rollout` is the honest signal a
/// consumer reads to know this cell is post-hoc and coarser than a hook-bridged TUI.
fn mark_rollout(body: &mut Value) {
    if let Value::Object(m) = body {
        m.insert("fidelity".into(), json!("rollout-import"));
        m.insert("source".into(), json!("rollout"));
    }
}

/// Map one codex TUI rollout record to an obs leaf + trimmed body, reusing the same
/// leaf vocabulary the live stream maps into (`assistant/message`,
/// `assistant/reasoning`, `tool/<n>/{call,result}`, `session/...`). Returns None
/// for records we deliberately drop (synthetic env/permissions context, redundant
/// `event_msg` mirrors of `response_item`s we already projected). Every returned
/// body carries the rollout-import provenance marker.
///
/// SCHEMA (verified against real `~/.codex/sessions` TUI rollouts, codex 0.140/0.141):
///   {type:"session_meta", payload:{id,cwd,originator,cli_version,...}}
///   {type:"event_msg",     payload:{type:"task_started"|"task_complete"|
///                                        "user_message"|"agent_message"|"token_count", ...}}
///   {type:"response_item", payload:{type:"message", role, content:[{type,text}]}}
///   {type:"response_item", payload:{type:"reasoning", summary:[], encrypted_content}}
///   {type:"response_item", payload:{type:"function_call", name, arguments, call_id}}
///   {type:"response_item", payload:{type:"function_call_output", call_id, output}}
///   {type:"turn_context",  payload:{...}}   (dropped — config echo, no obs value)
fn rollout_map_record(rec: &Value) -> Option<(String, Value)> {
    let ts = now_iso();
    let rtype = rec.get("type").and_then(Value::as_str).unwrap_or("");
    let payload = rec.get("payload").unwrap_or(&Value::Null);
    let ptype = payload.get("type").and_then(Value::as_str).unwrap_or("");
    let mut out = match (rtype, ptype) {
        // The codex thread id — file as session/thread (NOT a second session/start;
        // the launcher already emitted session/start), exactly like the live stream.
        ("session_meta", _) => Some((
            "session/thread".to_string(),
            json!({
                "ts": ts,
                "codex_thread": payload.get("id").cloned().unwrap_or(Value::Null),
                "cli_version": payload.get("cli_version").cloned().unwrap_or(Value::Null),
            }),
        )),
        // The genuine user prompt. event_msg/user_message is the CLEAN prompt (the
        // response_item message role=user carries the synthetic env/permissions
        // blocks too), so we project from event_msg and drop the role=user/developer
        // response_items below.
        ("event_msg", "user_message") => Some((
            "user/message".to_string(),
            json!({ "ts": ts, "text": clip_opt(payload.get("message"), 4000) }),
        )),
        // turn lifecycle / cost.
        ("event_msg", "task_started") => Some((
            "session/idle".to_string(),
            json!({ "ts": ts, "event": "task_started" }),
        )),
        ("event_msg", "task_complete") => Some((
            "session/idle".to_string(),
            json!({
                "ts": ts,
                "event": "task_complete",
                "last_agent_message": clip_opt(payload.get("last_agent_message"), 4000),
            }),
        )),
        ("event_msg", "token_count") => Some((
            "session/idle".to_string(),
            json!({ "ts": ts, "event": "token_count", "usage": clip_value(payload.get("info"), 2000) }),
        )),
        // event_msg/agent_message MIRRORS the response_item message role=assistant we
        // project below — drop it to avoid a duplicate assistant/message.
        ("event_msg", "agent_message") => None,
        // The assistant's settled message.
        ("response_item", "message") => {
            let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
            if role != "assistant" {
                // role=user / role=developer are synthetic env/permissions/context
                // (or the dup of the clean user_message) — drop.
                return None;
            }
            Some((
                "assistant/message".to_string(),
                json!({ "ts": ts, "text": clip(&rollout_message_text(payload), 4000) }),
            ))
        }
        // Reasoning trace. The TUI rollout usually carries ONLY encrypted_content
        // (no plaintext); project the summary text if any, else mark it redacted so
        // the bus shows a reasoning step happened without leaking ciphertext.
        ("response_item", "reasoning") => {
            let summary_text = rollout_reasoning_text(payload);
            Some((
                "assistant/reasoning".to_string(),
                json!({
                    "ts": ts,
                    "text": if summary_text.is_empty() { Value::Null } else { json!(clip(&summary_text, 4000)) },
                    "redacted": summary_text.is_empty(),
                }),
            ))
        }
        // A tool (shell/exec) call.
        ("response_item", "function_call") => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            Some((
                format!("tool/{}/call", topic::encode_segment(name)),
                json!({
                    "ts": ts,
                    "tool": name,
                    "call_id": payload.get("call_id").cloned().unwrap_or(Value::Null),
                    "arguments": clip_value(payload.get("arguments"), 2000),
                }),
            ))
        }
        // Its result. The rollout does not repeat the tool name on the output record
        // (only call_id), so file under a generic `tool/result` leaf carrying the
        // call_id to correlate with the matching call.
        ("response_item", "function_call_output") => Some((
            "tool/result".to_string(),
            json!({
                "ts": ts,
                "call_id": payload.get("call_id").cloned().unwrap_or(Value::Null),
                "output": clip_value(payload.get("output"), 4000),
            }),
        )),
        // turn_context is a per-turn config echo (cwd, approval/sandbox policy) — no
        // obs value; drop it.
        ("turn_context", _) => None,
        // Anything unmodeled still lands, tagged, so nothing is silently dropped.
        _ => {
            let tag = if ptype.is_empty() { rtype } else { ptype };
            Some((
                format!("rollout/{}", topic::encode_segment(tag)),
                json!({ "ts": ts, "rollout_type": rtype, "payload_type": ptype }),
            ))
        }
    };
    if let Some((_, body)) = out.as_mut() {
        mark_rollout(body);
    }
    out
}

/// Extract the plain text of a rollout `message` payload (concatenating its
/// content parts' `text`). Handles both `input_text` (user) and `output_text`
/// (assistant) parts.
fn rollout_message_text(payload: &Value) -> String {
    let Some(content) = payload.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    let mut s = String::new();
    for part in content {
        if let Some(t) = part.get("text").and_then(Value::as_str) {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(t);
        }
    }
    s
}

/// Extract any plaintext reasoning summary from a rollout `reasoning` payload. The
/// TUI rollout's `summary` is usually an empty array (the trace lives in
/// `encrypted_content`, which we never decrypt), so this is usually empty.
fn rollout_reasoning_text(payload: &Value) -> String {
    let Some(summary) = payload.get("summary").and_then(Value::as_array) else {
        return String::new();
    };
    let mut s = String::new();
    for part in summary {
        let t = part
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| part.as_str());
        if let Some(t) = t {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(t);
        }
    }
    s
}

/// Harvest the legible summary from one rollout record: the LAST settled assistant
/// message wins as `final_text` (the `task_complete.last_agent_message` is the
/// authoritative final word and overrides), and every shell/apply_patch the model
/// ran is left to the obs (the rollout does not carry a discrete file-change item
/// the way the live stream does, so we conservatively collect no paths here —
/// changed files still appear in the projected tool calls).
///
/// This rollout import is POST-HOC (read after the TUI exits) and carries no discrete
/// file-change item, so it is NOT the place to auto-claim a codex TUI's live edits.
/// The LIVE channel for that is codex's HOOK system: `PostToolUse` fires on
/// `apply_patch` even in the interactive TUI, delivering the patch on stdin (verified
/// against 0.141.0). The codex hook bridge (mirroring the Claude HookBridge) wires
/// that — see `codex` launch + `hook()`. This summary remains a legible-result
/// fallback; real-time "last editing <path>" rides the hook, not this import.
fn rollout_collect_summary(rec: &Value, summary: &mut CaptureSummary) {
    let payload = rec.get("payload").unwrap_or(&Value::Null);
    let rtype = rec.get("type").and_then(Value::as_str).unwrap_or("");
    let ptype = payload.get("type").and_then(Value::as_str).unwrap_or("");
    match (rtype, ptype) {
        ("response_item", "message")
            if payload.get("role").and_then(Value::as_str) == Some("assistant") =>
        {
            let text = rollout_message_text(payload);
            if !text.trim().is_empty() {
                summary.final_text = Some(clip(&text, FINAL_TEXT_CAP));
            }
        }
        // The authoritative final word — overrides any trailing assistant message.
        ("event_msg", "task_complete") => {
            if let Some(t) = payload.get("last_agent_message").and_then(Value::as_str) {
                if !t.trim().is_empty() {
                    summary.final_text = Some(clip(t, FINAL_TEXT_CAP));
                }
            }
        }
        _ => {}
    }
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
// OC5 — folding all three adapters into the HM1 capture seam. This adapter is
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
    injection: Option<&crate::provider::HarnessInjection>,
    skills: &[(String, PathBuf)],
) -> Result<(std::process::ExitStatus, CaptureSummary)> {
    use std::process::{Command, Stdio};

    let task = opencode_task_from_args(args)?;

    // M3: opencode scans `$OPENCODE_CONFIG_DIR/skills/<name>/SKILL.md` (verified
    // against 1.17.9 — it loaded a skill placed there and invoked it). So deliver
    // the profile's skills per-session by pointing OPENCODE_CONFIG_DIR at a scratch
    // dir of symlinked packages — ephemeral (removed at launch exit) and private to
    // the session, without touching the user's `~/.config/opencode` or repo. Built
    // only when the profile has skills; otherwise the env is left untouched.
    let oc_config_dir = (!skills.is_empty()).then(|| {
        let dir = root.run_dir().join(session).join("oc_config");
        if let Err(e) = link_skill_packages(&dir.join("skills"), skills) {
            eprintln!("[code] linking skills into opencode config dir: {e:#}");
        }
        dir
    });
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
    // M2: apply the named-provider injection (if any) after the scrub. For opencode
    // this sets OPENCODE_CONFIG_CONTENT (which outranks a stored login) and removes
    // inherited OPENCODE_CONFIG* so a parent's provider can't bleed into this child.
    if let Some(inj) = injection {
        apply_provider_injection_env(&mut cmd, inj);
    }
    cmd.env_remove("ELANUS_PACKAGE")
        .env_remove("ELANUS_BUS_TOKEN");
    // The session's own identity, carried to anything the opencode session spawns
    // (crucially `elanus code deliver`, which reads ELANUS_CODE_SESSION/AGENT).
    cmd.env(ENV_SESSION, session)
        .env(ENV_AGENT, agent)
        .env("ELANUS_ROOT", &root.dir);
    // M3: point opencode at the per-session skills config dir (Some only when the
    // profile had skills). Coexists with a `--provider` OPENCODE_CONFIG_CONTENT
    // injection (different config layer — dir = skills/agents, content = inline).
    if let Some(dir) = &oc_config_dir {
        cmd.env("OPENCODE_CONFIG_DIR", dir);
    }
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
    print_stream_worker_result(agent, session, &summary);

    let status = child.wait().context("waiting for opencode run to finish")?;
    Ok((status, summary))
}

// ── OC3: opencode TUI via a LIVE server SSE capture ───────────────────────────
//
// OBSERVED SSE CONTRACT — `opencode serve` + GET `/event` (v1.17.9).
// Discovered 2026-06-22 from the installed binary (no model call needed to read the
// shape; one trivial `opencode run --attach` confirmed the live content frames):
//
//   * `opencode serve` prints the listen URL on STDOUT:
//       `opencode server listening on http://127.0.0.1:<port>`
//     If `OPENCODE_SERVER_PASSWORD` is unset it warns "server is unsecured"; we set
//     a fresh random password so the loopback server requires basic auth (user
//     `opencode`, the default `OPENCODE_SERVER_USERNAME`).
//   * GET `/event` is a `text/event-stream`: frames are `data: <json>\n\n`. Each
//     decoded JSON is `{ "id": "evt_…", "type": "<dotted.kind>", "properties": {…} }`
//     — a DIFFERENT vocabulary from `opencode run --format json` (which emits settled
//     `{type:"text"|"tool_use"|…, part}` lines). The relevant SSE kinds:
//       - "server.connected" / "server.heartbeat"  → keepalives (ignored)
//       - "session.created"  → properties.info.id   = the native session id
//       - "message.part.updated" → properties.part  = a Part (the SAME Part shapes
//         that `run --format json` wraps): {type:"text"|"reasoning"|"tool"|
//         "step-start"|"step-finish", …}. A text/reasoning part is SETTLED when its
//         `time.end` is set; a tool part is settled when state.status is
//         completed/error.
//       - "session.idle"   → properties.sessionID   = the turn finished
//       - "session.error"  → properties.error
//   * `opencode attach <url>` runs the human's TUI against that server; its turns
//     flow through the SAME `/event` stream the launcher subscribes.
//
// REUSE: rather than write a second event mapper, the subscriber TRANSLATES a
// settled `message.part.updated` into the exact `{type, sessionID, part}` envelope
// `opencode_map_event` / `opencode_collect_summary` already consume (the Part shapes
// are identical), then reuses those. So the SSE projection and the headless stream
// projection share one mapping — the live cell and the headless cell land the SAME
// obs leaf vocabulary, differing only in the honest fidelity stamp.

/// OC3: launch opencode's interactive TUI captured LIVE off a served instance's SSE
/// stream (the live SSE cell). Steps:
///   1. Start our OWN `opencode serve` on a free port, basic-auth'd with a fresh
///      random `OPENCODE_SERVER_PASSWORD`, read its listen URL off stdout.
///   2. Spawn a background thread subscribing GET `/event` (SSE), projecting each
///      event into the obs grammar LIVE (`opencode_sse_publish` → `opencode_map_event`),
///      harvesting the native session id (→ durable record, so OC2 resume works) and
///      the legible summary.
///   3. Launch the human's TUI via `opencode attach <url>` with INHERITED stdio
///      inside the cage + the envelope briefing folded into the seed positional.
///   4. On TUI exit, kill the served instance (which EOFs the SSE stream) and join
///      the subscriber thread, returning the harvested summary.
///
/// FIDELITY: the SSE projection is LIVE (per-event) and stamped
/// `fidelity:"server-events-live"` / `source:"sse"` (see `opencode_sse_publish`),
/// distinct from codex's post-hoc `rollout-import`. Best-effort: the subscriber is
/// advisory telemetry — if `serve` fails to start or the stream drops, the TUI still
/// runs (uncaptured) and the launcher records the gap on the bus rather than killing
/// the human's session.
///
/// CONSTRUCTION-ONLY vs LIVE-TESTED: a real interactive `opencode attach` with a
/// human at a terminal cannot run headless, so the inherited-stdio TUI ergonomics are
/// verified by construction/inspection. The SSE CAPTURE MECHANISM itself is
/// live-testable headless (serve + drive a non-interactive `opencode run --attach`
/// against it + confirm the subscriber projects the events); the projection +
/// session-id harvest are also unit-tested on captured SSE samples.
#[allow(clippy::too_many_arguments)]
fn run_opencode_tui_server_events(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    workdir: &Path,
    args: &[String],
    brief: Option<&str>,
    injection: Option<&crate::provider::HarnessInjection>,
    skills: &[(String, PathBuf)],
) -> Result<(std::process::ExitStatus, CaptureSummary)> {
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // M3: the profile's skills in a per-session OPENCODE_CONFIG_DIR (same as the
    // headless cell). The SERVER is the process that loads skills, so the env goes
    // on the `serve` command below. None when the profile has no skills.
    let oc_config_dir = (!skills.is_empty()).then(|| {
        let dir = root.run_dir().join(session).join("oc_config");
        if let Err(e) = link_skill_packages(&dir.join("skills"), skills) {
            eprintln!("[code] linking skills into opencode config dir: {e:#}");
        }
        dir
    });

    // 1. Start our own served instance. A fresh per-launch password fences the
    //    loopback server (basic auth: user `opencode`, default username). `--port 0`
    //    asks opencode for a free port (it prints the resolved URL on stdout).
    let password = opencode_server_password();
    let mut serve = Command::new("opencode");
    serve
        .arg("serve")
        .arg("--port")
        .arg("0")
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    scrub_provider_creds(&mut serve);
    scrub_launch_control_env(&mut serve);
    // M2: the model runs in THIS served instance, so the named-provider injection
    // (OPENCODE_CONFIG_CONTENT) goes on `serve` — not the `attach` client. No-op
    // when no --provider.
    if let Some(inj) = injection {
        apply_provider_injection_env(&mut serve, inj);
    }
    serve
        .env_remove("ELANUS_PACKAGE")
        .env_remove("ELANUS_BUS_TOKEN")
        .env("OPENCODE_SERVER_PASSWORD", &password)
        .env(ENV_SESSION, session)
        .env(ENV_AGENT, agent)
        .env("ELANUS_ROOT", &root.dir);
    // M3: point the served instance at the per-session skills config dir. None when
    // the profile has no skills.
    if let Some(dir) = &oc_config_dir {
        serve.env("OPENCODE_CONFIG_DIR", dir);
    }

    let mut serve_child = serve
        .spawn()
        .with_context(|| "launching `opencode serve` (is opencode installed and on PATH?)")?;

    // Read the listen URL off the server's stdout (it prints
    // "opencode server listening on http://127.0.0.1:<port>"). We take stdout here
    // and keep the reader alive past the URL line so the pipe never fills and blocks
    // the server.
    let url = match serve_child.stdout.take().and_then(opencode_read_serve_url) {
        Some(u) => u,
        None => {
            // serve never announced a URL — degrade honestly: kill the half-started
            // server and fall back to a bracket-only TUI launch (no live capture),
            // recording the gap on the bus rather than aborting the human's session.
            let _ = serve_child.kill();
            let _ = serve_child.wait();
            publish_obs(
                root,
                principal,
                bus_token,
                &obs_topic(agent, session, "session/idle"),
                json!({
                    "ts": now_iso(),
                    "event": "server_events_unavailable",
                    "detail": "opencode serve did not announce a listen URL; TUI ran uncaptured",
                    "fidelity": "server-events-live",
                    "source": "sse",
                }),
            );
            eprintln!(
                "[code] opencode serve did not announce a URL; launching TUI WITHOUT live capture"
            );
            return run_opencode_attach_tui(
                root, agent, session, workdir, args, brief, None, &password, injection,
            );
        }
    };
    eprintln!("[code] opencode served at {url} (session {session}); subscribing SSE /event");

    // 2. Background SSE subscriber. `stop` lets us close the reqwest stream loop
    //    promptly once the TUI exits even if no more events arrive. The summary +
    //    native session id are harvested on the thread and returned on join.
    let stop = Arc::new(AtomicBool::new(false));
    let sub = {
        let root = root.clone();
        let principal = principal.to_string();
        let bus_token = bus_token.to_string();
        let agent = agent.to_string();
        let session = session.to_string();
        let workdir = workdir.to_path_buf();
        let url = url.clone();
        let password = password.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            opencode_subscribe_sse(
                &root, &principal, &bus_token, &agent, &session, &workdir, &url, &password, &stop,
            )
        })
    };

    // 3. The human's TUI, attached to the server we observe (inherited stdio).
    let tui = run_opencode_attach_tui(
        root,
        agent,
        session,
        workdir,
        args,
        brief,
        Some(&url),
        &password,
        injection,
    );

    // 4. Tear down: stop the subscriber loop, kill the served instance (EOFs the
    //    stream), join the subscriber for its harvested summary.
    stop.store(true, Ordering::SeqCst);
    let _ = serve_child.kill();
    let _ = serve_child.wait();
    let summary = sub.join().unwrap_or_default();

    let (status, _) = tui?;
    print_stream_worker_result(agent, session, &summary);
    Ok((status, summary))
}

/// Launch `opencode attach <url>` (or a bare `opencode` TUI when `url` is None — the
/// degraded no-capture fallback) with inherited stdio inside the cage, the envelope
/// briefing delivered as the session's opening prompt.
///
/// `opencode attach <url>` takes ONLY the `url` positional — it has no initial-message
/// argument (unlike the bare `opencode <message>` TUI). So in attach mode we can't fold
/// the seed into a positional; doing so makes opencode reject the extra arg and print
/// its usage. Instead we pre-create a session on the served instance and queue the seed
/// as its opening prompt over the server API, then `attach --session <id>` onto it. The
/// bare-TUI fallback (`url` = None) keeps using the positional, which it accepts.
///
/// Returns an empty summary — the live capture (when present) is harvested by the SSE
/// subscriber thread, not here.
#[allow(clippy::too_many_arguments)]
fn run_opencode_attach_tui(
    root: &Root,
    agent: &str,
    session: &str,
    workdir: &Path,
    args: &[String],
    brief: Option<&str>,
    url: Option<&str>,
    password: &str,
    injection: Option<&crate::provider::HarnessInjection>,
) -> Result<(std::process::ExitStatus, CaptureSummary)> {
    use std::process::Command;

    let task = opencode_task_from_args(args).unwrap_or_default();
    let seed = match (brief, task.trim().is_empty()) {
        (Some(b), false) => Some(opencode_message_with_brief(Some(b), &task)),
        (Some(b), true) => Some(opencode_message_with_brief(Some(b), "")),
        (None, false) => Some(task),
        (None, true) => None,
    };

    let mut cmd = Command::new("opencode");
    match url {
        Some(url) => {
            // Attach mode: deliver the seed over the server API (attach has no
            // initial-message positional), then continue that session in the TUI.
            cmd.arg("attach").arg(url);
            if let Some(seed) = seed.as_deref() {
                match opencode_seed_session(url, password, seed) {
                    Some(sid) => {
                        eprintln!("[code] opencode: seeded session {sid} with the opening prompt");
                        cmd.arg("--session").arg(sid);
                    }
                    None => eprintln!(
                        "[code] opencode: could not pre-seed session via API; attaching without the brief"
                    ),
                }
            }
        }
        None => {
            // Degraded fallback: bare `opencode <seed>` accepts an opening-message positional.
            if let Some(seed) = seed {
                cmd.arg(seed);
            }
        }
    }
    cmd.current_dir(workdir);
    cmd.stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    scrub_provider_creds(&mut cmd);
    scrub_launch_control_env(&mut cmd);
    // M2: in the degraded bare-TUI fallback (url=None) the model runs in THIS
    // process, so it needs the named-provider injection; in attach mode the served
    // instance already carries it and this is a harmless clean-child scrub. No-op
    // when no --provider.
    if let Some(inj) = injection {
        apply_provider_injection_env(&mut cmd, inj);
    }
    cmd.env_remove("ELANUS_PACKAGE")
        .env_remove("ELANUS_BUS_TOKEN");
    // attach reads the same basic-auth password to reach the server.
    cmd.env("OPENCODE_SERVER_PASSWORD", password);
    cmd.env(ENV_SESSION, session)
        .env(ENV_AGENT, agent)
        .env("ELANUS_ROOT", &root.dir);
    match url {
        Some(u) => eprintln!(
            "[code] launching opencode attach {u} as session {session} (live SSE capture)"
        ),
        None => eprintln!("[code] launching opencode TUI as session {session} (no live capture)"),
    }

    let status = cmd
        .status()
        .with_context(|| "launching opencode (is it installed and on PATH?)".to_string())?;
    Ok((status, CaptureSummary::default()))
}

/// Pre-create a session on the served opencode instance and queue `seed` (the envelope
/// brief folded with any task) as its opening prompt, returning the native session id so
/// the TUI can `attach --session <id>` onto it. `opencode attach <url>` has no
/// initial-message positional, so the seed must travel over the server API. Reaches the
/// loopback server with the same basic-auth password the TUI uses. Best-effort: returns
/// None on any failure (the caller then attaches to a fresh, un-seeded session).
fn opencode_seed_session(url: &str, password: &str, seed: &str) -> Option<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(async {
        let client = reqwest::Client::new();
        let created = client
            .post(format!("{url}/api/session"))
            .basic_auth("opencode", Some(password))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .ok()?;
        let body: serde_json::Value = created.json().await.ok()?;
        let sid = body.get("data")?.get("id")?.as_str()?.to_string();
        // Queue the seed so it survives even if the session is momentarily busy; admitted
        // immediately on a fresh session.
        let prompt = json!({ "prompt": { "text": seed }, "delivery": "queue" });
        let _ = client
            .post(format!("{url}/api/session/{sid}/prompt"))
            .basic_auth("opencode", Some(password))
            .header("content-type", "application/json")
            .body(prompt.to_string())
            .send()
            .await;
        Some(sid)
    })
}

/// A fresh per-launch basic-auth password for our served instance: prefer an
/// operator-provided `OPENCODE_SERVER_PASSWORD` (honor the user's setting), else mint
/// a random one so the loopback server is never the "unsecured" default.
fn opencode_server_password() -> String {
    if let Ok(p) = std::env::var("OPENCODE_SERVER_PASSWORD") {
        if !p.is_empty() {
            return p;
        }
    }
    // A nonce from the clock + pid — enough to fence a short-lived loopback server.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("elanus-{}-{}", std::process::id(), nanos)
}

/// Read `opencode serve`'s stdout until it announces its listen URL
/// (`… listening on http://127.0.0.1:<port>`); return that URL. Returns None if the
/// stream ends first (server failed to start). Drains remaining stdout in a detached
/// thread so the server's stdout pipe never fills and blocks it.
fn opencode_read_serve_url(out: std::process::ChildStdout) -> Option<String> {
    let mut reader = std::io::BufReader::new(out);
    let mut url = None;
    loop {
        let mut line = String::new();
        match std::io::BufRead::read_line(&mut reader, &mut line) {
            Ok(0) => break, // EOF before any URL
            Ok(_) => {
                if let Some(u) = opencode_extract_url(&line) {
                    url = Some(u);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if url.is_some() {
        // Keep draining so the server's stdout pipe never blocks it.
        std::thread::spawn(move || {
            let mut sink = String::new();
            let _ = std::io::Read::read_to_string(&mut reader, &mut sink);
        });
    }
    url
}

/// Extract the `http://host:port` URL from an `opencode serve` announcement line.
fn opencode_extract_url(line: &str) -> Option<String> {
    let idx = line.find("http://").or_else(|| line.find("https://"))?;
    let tail = &line[idx..];
    let end = tail.find(|c: char| c.is_whitespace()).unwrap_or(tail.len());
    let url = tail[..end].trim_end_matches('/').to_string();
    (!url.is_empty()).then_some(url)
}

/// Subscribe `opencode serve`'s SSE `/event` stream, projecting each event into the
/// obs grammar LIVE. Runs on a background thread with its own current-thread tokio
/// runtime (reqwest is async; the rest of `codeagent` is blocking). Reads the
/// `text/event-stream` chunk-by-chunk, splits `data: …\n\n` frames, and routes each
/// decoded event through `opencode_sse_publish`. Stops when the server EOFs the
/// stream (TUI torn down) or `stop` is set. Returns the harvested `CaptureSummary`.
/// Best-effort: any error (connect/read) records the gap on the bus and returns the
/// summary harvested so far rather than propagating.
#[allow(clippy::too_many_arguments)]
fn opencode_subscribe_sse(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    workdir: &Path,
    url: &str,
    password: &str,
    stop: &std::sync::atomic::AtomicBool,
) -> CaptureSummary {
    use std::sync::atomic::Ordering;

    let mut summary = CaptureSummary::default();
    let mut recorded = false;

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[code] opencode SSE: building runtime failed ({e}); TUI uncaptured");
            return summary;
        }
    };

    rt.block_on(async {
        let client = reqwest::Client::new();
        let resp = match client
            .get(format!("{url}/event"))
            .basic_auth("opencode", Some(password))
            .header("accept", "text/event-stream")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[code] opencode SSE subscribe failed ({e}); TUI runs uncaptured");
                publish_obs(
                    root,
                    principal,
                    bus_token,
                    &obs_topic(agent, session, "session/idle"),
                    json!({
                        "ts": now_iso(),
                        "event": "server_events_unavailable",
                        "detail": format!("SSE subscribe failed: {e}"),
                        "fidelity": "server-events-live",
                        "source": "sse",
                    }),
                );
                return;
            }
        };

        let mut resp = resp;
        // Accumulate bytes and split on the SSE frame terminator `\n\n`.
        let mut buf: Vec<u8> = Vec::new();
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    buf.extend_from_slice(&chunk);
                    while let Some(pos) = find_frame_end(&buf) {
                        let frame: Vec<u8> = buf.drain(..pos).collect();
                        // Drop the trailing `\n\n` separator from the buffer.
                        let drop = if buf.starts_with(b"\r\n\r\n") { 4 } else { 2 };
                        buf.drain(..drop.min(buf.len()));
                        if let Some(event) = parse_sse_frame(&frame) {
                            opencode_sse_publish(
                                root,
                                principal,
                                bus_token,
                                agent,
                                session,
                                workdir,
                                &event,
                                &mut summary,
                                &mut recorded,
                            );
                        }
                    }
                }
                Ok(None) => break, // server EOF (TUI torn down)
                Err(_) => break,
            }
        }
    });

    summary
}

/// Find the byte offset of the end of the first complete SSE frame in `buf` (the
/// index just before the `\n\n` or `\r\n\r\n` terminator). None if no complete frame
/// is buffered yet.
fn find_frame_end(buf: &[u8]) -> Option<usize> {
    let lf = find_subslice(buf, b"\n\n");
    let crlf = find_subslice(buf, b"\r\n\r\n");
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Parse one raw SSE frame (the bytes of `data: …` lines, terminator already
/// stripped) into the decoded event JSON. An SSE frame may carry multiple `data:`
/// lines (concatenated with `\n` per the spec); opencode emits one. Lines that are
/// not `data:` (e.g. `event:`/`id:`/comments) are ignored. Returns None for a frame
/// with no JSON `data` or unparseable JSON (a heartbeat comment, a partial frame).
fn parse_sse_frame(frame: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(frame);
    let mut data = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            // SSE allows an optional single leading space after the colon.
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest);
        }
    }
    if data.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&data).ok()
}

/// Project ONE decoded SSE event into the obs grammar LIVE, harvesting the native
/// session id (→ durable record) and the legible summary as it goes. Reuses the
/// headless mappers: a `message.part.updated` is TRANSLATED into the same
/// `{type, sessionID, part}` envelope `opencode_map_event`/`opencode_collect_summary`
/// consume (the Part shapes are identical), so the live cell lands the SAME obs leaf
/// vocabulary as the headless cell. Every published body is stamped
/// `fidelity:"server-events-live"` / `source:"sse"` so a consumer never mistakes the
/// live SSE capture for codex's post-hoc rollout import or a Claude hook bridge.
#[allow(clippy::too_many_arguments)]
fn opencode_sse_publish(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    workdir: &Path,
    event: &Value,
    summary: &mut CaptureSummary,
    recorded: &mut bool,
) {
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    // Keepalives and pure transport frames carry nothing for the bus.
    if matches!(kind, "server.connected" | "server.heartbeat") {
        return;
    }

    // Harvest the native session id (OC2 resume). It arrives on `session.created`
    // (properties.info.id) and is also present on every content event's part
    // (part.sessionID) — take the first we see and persist the durable record.
    if !*recorded {
        let sid = opencode_sse_native_id(event);
        if let Some(sid) = sid {
            if !sid.is_empty() {
                let rec = codesession::SessionRecord {
                    elanus_session: session.to_string(),
                    native_session: sid,
                    tool: "opencode".to_string(),
                    agent_noun: agent.to_string(),
                    workdir: workdir.display().to_string(),
                    room: None,
                };
                if let Err(e) = codesession::upsert_record(root, &rec) {
                    eprintln!("[code] recording opencode TUI session (continuing): {e:#}");
                }
                *recorded = true;
            }
        }
    }

    // Translate the SSE event into the headless `run --format json` envelope shape,
    // then reuse the existing mappers so the live and headless cells share one
    // projection. `None` = an SSE-only control frame we surface directly below.
    if let Some(run_event) = opencode_sse_to_run_event(event) {
        opencode_collect_summary(&run_event, summary);
        // SA3 write half for the LIVE TUI cell (the gap journey 12's adjudication
        // flagged): a settled `edit`/`write` part is a file write — auto-claim it,
        // parity with the headless `run` path, so a roommate sees what an
        // INTERACTIVE opencode session is editing in REAL TIME, not just a headless
        // worker. cwd=None → resolved from the session's recorded workdir (upserted
        // above). Advisory + idempotent.
        if let Some(path) = opencode_file_write_path(&run_event) {
            auto_claim_write(root, session, path, None);
        }
        for (leaf, body) in opencode_map_event(&run_event) {
            publish_obs(
                root,
                principal,
                bus_token,
                &obs_topic(agent, session, &leaf),
                stamp_sse_fidelity(body),
            );
        }
        return;
    }

    // SSE-only control frames with no headless analog: surface them on the idle leaf
    // so the live stream is legible (session boundaries / errors), nothing dropped.
    match kind {
        "session.idle" => publish_obs(
            root,
            principal,
            bus_token,
            &obs_topic(agent, session, "session/idle"),
            stamp_sse_fidelity(json!({ "ts": now_iso(), "event": "session.idle" })),
        ),
        "session.error" => publish_obs(
            root,
            principal,
            bus_token,
            &obs_topic(agent, session, "session/idle"),
            stamp_sse_fidelity(json!({
                "ts": now_iso(),
                "event": "session.error",
                "error": clip_value(event.get("properties").and_then(|p| p.get("error")), 4000),
            })),
        ),
        // session.created etc. already harvested the id above; nothing else to file.
        _ => {}
    }
}

/// The native opencode session id carried by an SSE event, if any: `session.created`
/// puts it under `properties.info.id`; content events carry it under
/// `properties.sessionID` (and on the nested part).
fn opencode_sse_native_id(event: &Value) -> Option<String> {
    let props = event.get("properties")?;
    if let Some(id) = props
        .get("info")
        .and_then(|i| i.get("id"))
        .and_then(Value::as_str)
    {
        return Some(id.to_string());
    }
    props
        .get("sessionID")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Translate a settled SSE `message.part.updated` event into the `{type, sessionID,
/// part}` envelope the headless `opencode_map_event`/`opencode_collect_summary`
/// consume, so the live SSE cell reuses the headless projection verbatim. Returns
/// None for events with no headless analog (handled directly by `opencode_sse_publish`)
/// and for UNSETTLED parts (a text/reasoning part with no `time.end`, a tool part not
/// yet completed/error) — matching the headless stream, which only emits SETTLED
/// parts. The Part `type` is remapped to the headless `type` value:
///   text → "text", reasoning → "reasoning", tool → "tool_use",
///   step-start → "step_start", step-finish → "step_finish".
fn opencode_sse_to_run_event(event: &Value) -> Option<Value> {
    if event.get("type").and_then(Value::as_str)? != "message.part.updated" {
        return None;
    }
    let part = event.get("properties").and_then(|p| p.get("part"))?;
    let ptype = part.get("type").and_then(Value::as_str)?;
    let session_id = part
        .get("sessionID")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let run_type = match ptype {
        "text" | "reasoning" => {
            // Settled only: the headless stream emits a text/reasoning event only
            // once part.time.end is set. Mirror that so a live partial isn't filed
            // (and re-filed) as a final message.
            let settled = part.get("time").and_then(|t| t.get("end")).is_some();
            if !settled {
                return None;
            }
            if ptype == "text" {
                "text"
            } else {
                "reasoning"
            }
        }
        "tool" => {
            // Settled only: completed or error (matches the headless `tool_use`).
            let status = part
                .get("state")
                .and_then(|s| s.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if status != "completed" && status != "error" {
                return None;
            }
            "tool_use"
        }
        "step-start" => "step_start",
        "step-finish" => "step_finish",
        // Other part kinds (file/snapshot/patch/agent/…) have no headless analog.
        _ => return None,
    };

    Some(json!({
        "type": run_type,
        "sessionID": session_id,
        "part": part.clone(),
    }))
}

/// Stamp a projected body with the LIVE-SSE fidelity markers so a consumer knows the
/// capture is live per-event (distinct from codex's post-hoc `rollout-import` and a
/// Claude hook bridge). A no-op for a non-object body (never happens for our maps).
fn stamp_sse_fidelity(mut body: Value) -> Value {
    if let Value::Object(m) = &mut body {
        m.insert("fidelity".into(), json!("server-events-live"));
        m.insert("source".into(), json!("sse"));
    }
    body
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
        // SA3 (write half): a settled `edit`/`write` tool_use is a file write —
        // auto-claim its `path` for this session in its room, so a roommate sees it
        // without this agent running `elanus code claim`. Reuses the SAME
        // edit|write + state.input.path detection opencode_collect_summary uses;
        // advisory + idempotent.
        if let Some(path) = opencode_file_write_path(&event) {
            auto_claim_write(
                root,
                session,
                path,
                record_workdir.map(|w| w.to_string_lossy()).as_deref(),
            );
        }
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
        // SI1 (sibling-intent): opencode is hookless too — refresh `last_active`
        // once per stream event so a long-running opencode session stays "live"
        // to its siblings between resumes. Best-effort.
        let _ = codesession::bump_last_active(root, session);
    }
    summary
}

/// Harvest the legible result from one opencode stream event into `summary`: the
/// text of each settled `text` event (so the LAST one wins — the verbatim final
/// answer, capped/marked) and the file path of each settled file-writing
/// `tool_use`. opencode's built-in `edit`/`write` tools key the changed file under
/// `state.input.filePath` (VERIFIED against a real `opencode run --format json`
/// stream, opencode 1.17.9 — see `opencode_tool_file_path`). Reads the SAME settled
/// events `opencode_map_event` files as obs; collecting here keeps that mapping
/// untouched.
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
            if let Some(path) = opencode_tool_file_path(event.get("part")) {
                summary.note_change(path);
            }
        }
        _ => {}
    }
}

/// The changed file path carried by a settled opencode `edit`/`write` tool part, or
/// `None` for any other tool / a missing path. opencode's real headless stream keys
/// the file under `state.input.filePath` (VERIFIED against opencode 1.17.9 — the
/// read/edit/write tools all carry `state.input.filePath`); a legacy `path` is
/// accepted as a fallback so an older binary still harvests. Shared by the summary
/// harvest and the SA3 auto-claim so the two never drift on the field name. The
/// headless `run --format json` envelope and the SSE-normalized envelope
/// (`opencode_sse_to_run_event` clones `part` verbatim) are identical here, so this
/// one extractor fixes both the headless and the live SSE/TUI projection.
fn opencode_tool_file_path(part: Option<&Value>) -> Option<&str> {
    let tool = part
        .and_then(|p| p.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !opencode_is_file_writer(tool) {
        return None;
    }
    let input = part
        .and_then(|p| p.get("state"))
        .and_then(|s| s.get("input"))?;
    input
        .get("filePath")
        .or_else(|| input.get("path"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// SA3 (write half): the file path of a settled opencode `edit`/`write` `tool_use`
/// (the same `type:"tool_use"` → file-writer tool → `state.input.filePath` shape
/// `opencode_collect_summary` harvests, via the shared `opencode_tool_file_path`),
/// for auto-claiming. `None` for any other event. Kept separate from the summary
/// harvest so the obs/summary mapping is untouched.
fn opencode_file_write_path(event: &Value) -> Option<&str> {
    if event.get("type").and_then(Value::as_str) != Some("tool_use") {
        return None;
    }
    opencode_tool_file_path(event.get("part"))
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
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

fn write_capture_summary_file(path: Option<&Path>, summary: &CaptureSummary) {
    let Some(path) = path else {
        return;
    };
    if let Err(e) = std::fs::write(path, serde_json::to_string(summary).unwrap_or_default()) {
        eprintln!(
            "[code] writing adapter summary {} failed: {e:#}",
            path.display()
        );
    }
}

fn read_capture_summary_file(path: Option<&Path>) -> Option<CaptureSummary> {
    let path = path?;
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Print the legible result of a blocking Codex launch to the caller's stdout.
/// The same summary has already been harvested while publishing obs; this is the
/// in-band surface a live parent can read without any bus authority. Keep the
/// format marked and plain so another tool can scrape it if needed.
fn print_stream_worker_result(tool: &str, session: &str, summary: &CaptureSummary) {
    println!("=== {tool} worker result (session {session}) ===");
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
        // SA3 (write half): each settled `file_change` is a write — auto-claim the
        // path(s) for this session in its room, so a roommate sees them without
        // this agent running `elanus code claim`. Reuses the SAME `file_change`
        // detection codex_collect_summary uses; advisory + idempotent.
        for path in codex_file_change_paths(&event) {
            auto_claim_write(
                root,
                session,
                &path,
                record_workdir.map(|w| w.to_string_lossy()).as_deref(),
            );
        }
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
            // SI1 (sibling-intent): refresh once per mapped stream event so a
            // long-running codex exec session stays "live" to its siblings.
            // Best-effort.
            let _ = codesession::bump_last_active(root, session);
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

/// SA3 (write half): extract the file paths of a settled codex `file_change` item
/// (the same `item.completed` → `file_change` → `changes[].path` shape
/// `codex_collect_summary` harvests), for auto-claiming. Empty for any other
/// event. Kept separate from the summary harvest so the obs/summary mapping is
/// untouched.
fn codex_file_change_paths(event: &Value) -> Vec<String> {
    if event.get("type").and_then(Value::as_str) != Some("item.completed") {
        return Vec::new();
    }
    let Some(item) = event.get("item") else {
        return Vec::new();
    };
    if item.get("type").and_then(Value::as_str) != Some("file_change") {
        return Vec::new();
    }
    item.get("changes")
        .and_then(Value::as_array)
        .map(|changes| {
            changes
                .iter()
                .filter_map(|c| c.get("path").and_then(Value::as_str))
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn codex_apply_patch_paths(command: &str) -> Vec<String> {
    const PREFIXES: &[&str] = &[
        "*** Add File:",
        "*** Update File:",
        "*** Delete File:",
        "*** Move to:",
    ];
    command
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            PREFIXES.iter().find_map(|prefix| {
                let path = line.strip_prefix(prefix)?.trim();
                (!path.is_empty()).then(|| path.to_string())
            })
        })
        .collect()
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
fn resume_command_for(rec: &codesession::SessionRecord, message: &str) -> (String, Vec<String>) {
    match harness_id_for_tool(&rec.tool).unwrap_or("claude") {
        "claude" => (
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
        _ => unreachable!("harness_id_for_tool only yields known tool ids"),
    }
}

fn resume_stream_capture_for(
    root: &Root,
    principal: &str,
    bus_token: &str,
    agent: &str,
    session: &str,
    rec: &codesession::SessionRecord,
    child: &mut std::process::Child,
) -> CaptureSummary {
    match harness_id_for_tool(&rec.tool).unwrap_or("claude") {
        "claude" => capture_claude_stream(root, principal, bus_token, agent, session, child),
        "codex" => capture_codex_stream(root, principal, bus_token, agent, session, child, None),
        "opencode" => capture_opencode_stream(root, principal, bus_token, agent, session, child, None),
        _ => unreachable!("harness_id_for_tool only yields known tool ids"),
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

// NOTE: there is deliberately NO human `elanus code resume` verb. "Resume" is not
// an elanus primitive — re-attaching to a session interactively is just a normal
// managed launch with the tool's own resume flag passed through, e.g.
// `elanus code claude --resume <native_session>` (hooks/room/obs all intact). The
// webui/`elanus code session` surface that per-tool suggestion via
// `interactive_resume_hint`. The only resume that lives in elanus is the daemon's
// async one-shot, `resume_capture` below (M2-B), driven IN-PROCESS off a mailbox
// delivery — never a command a human types.

/// The structured result of one driven/CLI resume — enough for the daemon to
/// thread a completion obs and settle the delivery event without ever exiting.
/// `final_text` + `file_changes` are the worker's LEGIBLE result (its verbatim last
/// message and the files it wrote), harvested from the capture stream so the routed
/// completion carries the worker's real answer (M4-A follow-on) — not a summary.
#[derive(Debug)]
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
    let token = codesession::mint(
        root,
        &principal,
        &rec.agent_noun,
        std::process::id() as i32,
        None,
        codesession::RequestedGrants::default(),
    )
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

    let (program, cmd_args) = resume_command_for(&rec, &injected);
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

        // Each adapter's resume emits a JSONL stream on stdout; the harness owns
        // reading it (codex/opencode reuse their launch-stream readers, claude uses
        // the CC `-p --output-format stream-json` mapper). The daemon resolves the
        // capture path from the recorded tool.
        summary = resume_stream_capture_for(
            root,
            &principal,
            &bus_token,
            &agent,
            &session,
            &rec,
            &mut child,
        );
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

fn map_hook_event(noun: &str, event: &str, payload: &Value) -> (String, Value) {
    match noun {
        n if n == claude_agent_noun() => claude_map_event(event, payload),
        n if n == codex_agent_noun() || n == opencode_agent_noun() => {
            generic_event(event, payload)
        }
        _ => generic_event(event, payload),
    }
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

    let (Ok(session), Ok(agent)) = (std::env::var(ENV_SESSION), std::env::var(ENV_AGENT)) else {
        // Outside a launched session (no identity in the env): nothing to file,
        // and we must not fail the coding session. Stay quiet.
        return Ok(());
    };
    let principal = std::env::var("ELANUS_PACKAGE")
        .ok()
        .filter(|s| !s.is_empty());
    let token = std::env::var("ELANUS_BUS_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());

    // The DURABLE session record (M2-A): Claude Code carries its own native
    // resumable session id in every hook payload (`session_id`). On SessionStart —
    // the first hook of a run — persist the record (elanus session ↔ CC session_id
    // ↔ workdir), so the session is resumable (`claude -p --resume <session_id>`)
    // even after the launcher exits. The record carries no secret. Best-effort: a
    // failure here must never break the hook or the coding session.
    if matches!(event, "SessionStart" | "Setup") && agent == claude_agent_noun() {
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
    // session's agent noun. Codex hooks are claim-only today: even though the child
    // has the SDK launch contract, this hook path deliberately avoids publishing
    // codex observations.
    if agent != codex_agent_noun() {
        if let (Some(principal), Some(token)) = (principal.as_deref(), token.as_deref()) {
            let (leaf, body) = map_hook_event(&agent, event, &payload);
            publish_obs(
                root,
                principal,
                token,
                &obs_topic(&agent, &session, &leaf),
                body,
            );
        }
    }

    // SI1 (sibling-intent): keep `code_sessions.last_active` genuinely fresh from
    // the per-event capture path. Claude fires this hook per tool event, so a
    // long-running session never reads as stranded to a sibling between resumes.
    // Best-effort: a bump failure must never break the hook or the session.
    let _ = codesession::bump_last_active(root, &session);

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
    // GATED on the advisory read-camera toggle (sandbox.read_camera, default ON):
    // when OFF, M1 STOPS publishing read events — "off" is a real, legible state
    // (read-provenance M3), not cosmetic — and the broker fast-fails any subscribe
    // to the read flavor so a consumer never reads silence as "no reads happened".
    // The AUTHORITATIVE tier (M2, the cage/syscall read camera) stays deferred.
    if event == "PreToolUse" && agent == claude_agent_noun() && read_camera_enabled(root) {
        for (topic_name, fs_body) in claude_read_fs_events(&payload, &session) {
            if let (Some(principal), Some(token)) = (principal.as_deref(), token.as_deref()) {
                publish_obs(root, principal, token, &topic_name, fs_body);
            }
        }
    }

    // SA3 (write half) — touching a file IS the claim. On a Claude-Code
    // file-write tool call (Write/Edit/MultiEdit/NotebookEdit), record an advisory
    // edit-claim for this session on that path in its room, so a roommate sees it
    // WITHOUT this agent ever running `elanus code claim`. Same PreToolUse hook the
    // M1 read camera rides; advisory + idempotent (see auto_claim_write). The READ
    // half (auto-claim on Read/Grep/Glob) is DEFERRED: it rides the authoritative
    // read camera (read-provenance M2), not built — claiming every file an agent
    // merely READS would be a firehose and the M1 read tier is honest-agent-only.
    if event == "PreToolUse" && agent == claude_agent_noun() {
        let tool = tool_name(&payload);
        if let Some(path) = claude_write_tool_path(&tool, payload.get("tool_input")) {
            let cwd = payload.get("cwd").and_then(Value::as_str);
            auto_claim_write(root, &session, path, cwd);
        }
    }

    let hook_event = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or(event);
    if hook_event == "PostToolUse"
        && agent == codex_agent_noun()
        && tool_name(&payload) == "apply_patch"
    {
        let cwd = payload.get("cwd").and_then(Value::as_str);
        if let Some(command) = payload
            .get("tool_input")
            .and_then(|i| i.get("command"))
            .and_then(Value::as_str)
        {
            let ctx = crate::harness::Ctx::from_env().ok();
            for path in codex_apply_patch_paths(command) {
                if let Some(ctx) = &ctx {
                    ctx.claim(&path);
                } else {
                    auto_claim_write(root, &session, &path, cwd);
                }
            }
        }
    }

    // DEFERRED (memory-blocks M4 follow-ons, intentionally NOT built here):
    //   1. opencode SERVED mid-cycle push — the spike proved `POST
    //      /session/{id}/prompt_async` can inject mid-turn from the SSE subscriber
    //      (the same server API `opencode_seed_session` POSTs to). When wired,
    //      `achievable_vector("opencode", MidCycle)` would return MidCycle for the
    //      served/TUI cell. Until then opencode-headless degrades to next-turn.
    //   2. the ALGEDONIC / signal-plane interrupt — opencode `POST /session/{id}/abort`
    //      + inject is a true drop-everything interrupt onto the existing `signal/`
    //      plane. Out of M4's scope: M4 ships next-turn-everywhere + Claude-Code
    //      mid-cycle + the degradation matrix.
    //
    // M4 MID-CYCLE block vector (memory-blocks handoff) — Claude Code only, the
    // spike-proven path: on PreToolUse/PostToolUse, emit any pending HIGH-PRIORITY
    // (mid-cycle) memory block for this session as `hookSpecificOutput`
    // `additionalContext`, so a louder block reaches the model BETWEEN tool calls,
    // not just next-turn. De-duped content-addressably (`take_pending_mid_cycle`):
    // an unchanged block emits ONCE, not on every tool call; editing it re-arms a
    // single redelivery. Codex/opencode have no live hook bridge → a mid-cycle
    // block DEGRADES to next-turn there (achievable_vector), surfaced by the normal
    // turn_injection — this arm is reached only for Claude Code (the only harness
    // whose hook fires). The dedup write makes this safe to call every tool event.
    if matches!(event, "PreToolUse" | "PostToolUse" | "PostToolUseFailure")
        && agent == claude_agent_noun()
    {
        // Sanity: this IS the achievable mid-cycle vector for Claude Code.
        if achievable_vector(&agent, InjectionVector::MidCycle) == InjectionVector::MidCycle {
            let mut ctx_parts: Vec<String> = Vec::new();
            if let Some(ctx) = mid_cycle_injection(root, &agent, &session) {
                ctx_parts.push(ctx);
            }
            // C3 (agent-comms) — HIGH-priority UNSEEN inbox mail reaches the model
            // mid-cycle too, not just next-turn. Threshold from config. De-duped by
            // event id (`code_mail_delivered`) so the same message is injected ONCE,
            // not on every tool call — and NOT marked seen (the agent has not pulled
            // it, so it still counts in the next-turn inbox block). Codex/opencode
            // have no live hook bridge, so this arm is reached only for Claude Code;
            // there the mail simply stays next-turn via the inbox block.
            if let Some(mail) = mid_cycle_mail_injection(root, &agent, &session) {
                ctx_parts.push(mail);
            }
            if !ctx_parts.is_empty() {
                let out = json!({
                    "hookSpecificOutput": {
                        "hookEventName": event,
                        "additionalContext": ctx_parts.join("\n"),
                    }
                });
                println!("{out}");
            }
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

    // E3 (work-estimation handoff) — RETRO on session end. When a session declared
    // an estimate (E1), close the loop: compute the actual-vs-estimate variance and
    // append the dated miss to the durable `estimation` block (agent scope), the
    // default-that-evolves memory a future E1 reads. This is the Stop-driven retro
    // (the spec's MVP — the LLM "why it missed" reflection is the documented
    // follow-on). Best-effort and once-per-session: a session with no estimate is
    // skipped, and `estimate_retro_once` guards against the several Stop/SessionEnd
    // events one run can emit so the block gains exactly one entry per session. Any
    // failure here is swallowed — the retro must never break the coding session.
    if matches!(event, "Stop" | "StopFailure" | "SessionEnd") {
        estimate_retro_once(root, &agent, &session);
    }
    Ok(())
}

/// E3 — run the work-estimation retro for this coding session. `estimatecli::retro`
/// is itself once-per-session (it sets a `estimation-retro-done` marker), so the
/// several Stop/SessionEnd events one run emits record exactly one miss, and a cron
/// backstop calling the same path stays idempotent. Everything is best-effort: a
/// missing estimate, an absent projection, or any error simply leaves the durable
/// `estimation` block untouched — the retro is advisory and must never break the
/// coding session.
fn estimate_retro_once(root: &Root, agent: &str, session: &str) {
    let opts = crate::estimatecli::EstimateOpts {
        profile: "default".into(),
        session: session.to_string(),
        owner: Some(agent.to_string()),
        pricing: None,
    };
    let _ = crate::estimatecli::retro(root, &opts);
}

/// Map a Claude Code hook event + its stdin payload to an obs/ topic leaf and a
/// trimmed body. The grammar matches src/exec.rs:
/// `tool/<name>/{call,result}` for the tool loop, plus session/turn leaves.
/// The hook stdin payload includes `session_id`, `cwd`, `permission_mode`,
/// `hook_event_name`, plus event-specific fields (Appendix A). The Codex adapter
/// adds a sibling `codex_map_event` and its own hook-mapping helper.
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

/// Is the ADVISORY read camera (M1) enabled? Reads the system `sandbox.read_camera`
/// toggle (default ON) off the `default` profile — the same whole-system config home
/// `profile::mailboxes` reads. When OFF, the M1 read-event projection is suppressed at
/// the call site so "off" is a real, observable state (read-provenance M3): no read
/// events on the bus, and the broker fast-fails read-flavor subscribes. A failure to
/// load the profile falls back to the default (ON) — the toggle is a deliberate opt-OUT,
/// not a fragile opt-in.
fn read_camera_enabled(root: &Root) -> bool {
    crate::profile::load(root, "default")
        .map(|(p, _)| p.sandbox.read_camera)
        .unwrap_or(true)
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
    let tool_use_id = payload.get("tool_use_id").cloned().unwrap_or(Value::Null);
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
pub fn publish_obs(root: &Root, principal: &str, token: &str, topic_name: &str, body: Value) {
    // buscli::publish reads ELANUS_PACKAGE/ELANUS_BUS_TOKEN from the environment.
    // In the launcher process those aren't set (only the child's were), so set
    // them for this publish; the hook process already has them. Setting them
    // unconditionally keeps both call sites correct.
    std::env::set_var("ELANUS_PACKAGE", principal);
    std::env::set_var("ELANUS_BUS_TOKEN", token);
    let payload = body.to_string();
    // buscli::publish builds its own current-thread runtime and `block_on`s it. If we
    // are ALREADY inside a tokio runtime — e.g. the opencode SSE subscriber projecting a
    // live event off its `block_on` loop — nesting another `block_on` panics ("Cannot
    // start a runtime from within a runtime"). Offload to a fresh OS thread, which has no
    // runtime entered, so the publish's own runtime is the only one on that thread.
    let result = if tokio::runtime::Handle::try_current().is_ok() {
        let root = root.clone();
        let topic = topic_name.to_string();
        std::thread::spawn(move || buscli::publish(&root, &topic, Some(&payload), 0, false, None))
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("obs publish thread panicked")))
    } else {
        buscli::publish(root, topic_name, Some(&payload), 0, false, None)
    };
    if let Err(e) = result {
        eprintln!("[code] obs publish to {topic_name} failed (continuing): {e:#}");
    }
}

/// Session-scoped observation topic: obs/agent/<agent>/<session>/<leaf>. Mirrors
/// src/exec.rs `obs()` exactly so coding-session telemetry shares the grammar.
pub fn obs_topic(agent: &str, session: &str, leaf: &str) -> String {
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
    fn tool_aliases_normalize_for_dispatch() {
        assert_eq!(harness_id_for_tool("claude"), Some("claude"));
        assert_eq!(harness_id_for_tool("claude-code"), Some("claude"));
        assert_eq!(harness_id_for_tool("cc"), Some("claude"));
        assert_eq!(harness_id_for_tool("codex"), Some("codex"));
        assert_eq!(harness_id_for_tool("opencode"), Some("opencode"));
        assert_eq!(harness_id_for_tool("oc"), Some("opencode"));
        assert!(harness_id_for_tool("nonsense").is_none());
    }

    #[test]
    fn map_hook_event_and_resume_command_are_free_functions() {
        let payload = json!({
            "session_id": "cc-123",
            "cwd": "/tmp/proj",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": "ls -la" },
        });
        let (leaf, body) = map_hook_event(claude_agent_noun(), "PreToolUse", &payload);
        assert_eq!(leaf, "tool/Bash/call");
        assert_eq!(body["tool"], "Bash");
        assert_eq!(body["cc_session"], "cc-123");

        let rec = codesession::SessionRecord {
            elanus_session: "code-aaaa1111".to_string(),
            native_session: "019ee252-3d31-7681-b1d7-7a4b3c494fb5".to_string(),
            tool: "codex".to_string(),
            agent_noun: codex_agent_noun().to_string(),
            workdir: "/tmp/proj".to_string(),
            room: None,
        };
        let (prog, args) = resume_command_for(&rec, "say hi again");
        assert_eq!(prog, "codex");
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "resume");
        assert_eq!(args[2], rec.native_session);

        let mut rec = rec.clone();
        rec.tool = "nonesuch".to_string();
        let (prog, args) = resume_command_for(&rec, "fallback");
        assert_eq!(prog, "claude");
        assert!(args.contains(&"--resume".to_string()));
    }

    #[test]
    fn split_codex_seed_prompt_separates_flags_from_prompt() {
        // Plain prompt, no flags.
        let (flags, prompt) = split_codex_seed_prompt(&["do a thing".to_string()]);
        assert!(flags.is_empty());
        assert_eq!(prompt.as_deref(), Some("do a thing"));
        // Value-taking flag passed through; prompt still found.
        let (flags, prompt) = split_codex_seed_prompt(&[
            "-m".to_string(),
            "gpt-5".to_string(),
            "the task".to_string(),
        ]);
        assert_eq!(flags, vec!["-m".to_string(), "gpt-5".to_string()]);
        assert_eq!(prompt.as_deref(), Some("the task"));
        // No prompt at all (bare TUI).
        let (flags, prompt) = split_codex_seed_prompt(&[]);
        assert!(flags.is_empty());
        assert!(prompt.is_none());
    }

    // ── HM2: codex rollout import — reader against a real-schema fixture ──────

    /// A SMALL, TRIMMED, SANITIZED copy of a real codex TUI rollout JSONL captured
    /// (read-only) from `~/.codex/sessions` — the exact on-disk schema codex 0.140/
    /// 0.141 writes (`session_meta` / `event_msg` / `response_item` / `turn_context`
    /// records). Secrets and real paths are scrubbed; the event SHAPES are verbatim
    /// so the reader is tested against the format it must parse in the field.
    const ROLLOUT_FIXTURE: &str = r#"{"timestamp":"2026-06-17T02:20:30.640Z","type":"session_meta","payload":{"id":"019ed361-4fd2-7793-9ea6-a1add8ca3d4f","timestamp":"2026-06-17T02:20:30.572Z","cwd":"/work/proj","originator":"codex-tui","cli_version":"0.140.0","source":"tui"}}
{"timestamp":"2026-06-17T02:20:30.640Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}
{"timestamp":"2026-06-17T02:20:32.147Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>...synthetic...</permissions instructions>"}]}}
{"timestamp":"2026-06-17T02:20:32.147Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context><cwd>/work/proj</cwd></environment_context>"}]}}
{"timestamp":"2026-06-17T02:20:32.154Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"List the files in this directory."}]}}
{"timestamp":"2026-06-17T02:20:32.154Z","type":"event_msg","payload":{"type":"user_message","message":"List the files in this directory.","images":[]}}
{"timestamp":"2026-06-17T02:20:35.201Z","type":"turn_context","payload":{"turn_id":"t1","cwd":"/work/proj","approval_policy":"never","sandbox_policy":{"type":"read-only"}}}
{"timestamp":"2026-06-17T02:20:35.201Z","type":"response_item","payload":{"type":"reasoning","summary":[],"encrypted_content":"REDACTED_CIPHERTEXT"}}
{"timestamp":"2026-06-17T02:20:36.221Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"I'll list the directory now."}],"phase":"commentary"}}
{"timestamp":"2026-06-17T02:20:37.528Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls\",\"workdir\":\"/work/proj\"}","call_id":"call_ABC"}}
{"timestamp":"2026-06-17T02:20:37.613Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_ABC","output":"Process exited with code 0\nOutput:\nCargo.toml\nsrc\n"}}
{"timestamp":"2026-06-17T02:20:37.613Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":27434}}}}
{"timestamp":"2026-06-17T02:20:40.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"There are two entries: Cargo.toml and src."}],"phase":"final_answer"}}
{"timestamp":"2026-06-17T02:21:04.266Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","last_agent_message":"There are two entries: Cargo.toml and src."}}
"#;

    /// Project a rollout record to its (leaf, body) WITHOUT publishing — the pure
    /// mapper, for assertions.
    fn map_line(line: &str) -> Option<(String, Value)> {
        rollout_map_record(&serde_json::from_str(line).unwrap())
    }

    #[test]
    fn rollout_reader_projects_fixture_into_obs_grammar_with_provenance() {
        // Collect every projected (leaf, body) from the fixture, in order.
        let projected: Vec<(String, Value)> = ROLLOUT_FIXTURE
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(map_line)
            .collect();
        let leaves: Vec<&str> = projected.iter().map(|(l, _)| l.as_str()).collect();

        // The synthetic developer/env user messages and the turn_context are DROPPED;
        // the redundant event_msg/agent_message mirror is absent (there is none here,
        // but the role=user response_items are dropped in favor of the clean
        // user_message). The projected sequence reuses the live leaf vocabulary.
        assert_eq!(
            leaves,
            vec![
                "session/thread",         // session_meta → thread id
                "session/idle",           // task_started
                "user/message",           // event_msg/user_message (the clean prompt)
                "assistant/reasoning",    // reasoning (redacted ciphertext)
                "assistant/message",      // assistant commentary
                "tool/exec_command/call", // function_call
                "tool/result",            // function_call_output
                "session/idle",           // token_count
                "assistant/message",      // assistant final answer
                "session/idle",           // task_complete
            ],
            "projected leaves: {leaves:?}"
        );

        // EVERY projected body carries the honest post-hoc provenance marker so a
        // consumer never mistakes rollout import for live granularity.
        for (leaf, body) in &projected {
            assert_eq!(
                body.get("fidelity").and_then(Value::as_str),
                Some("rollout-import"),
                "{leaf} missing fidelity marker"
            );
            assert_eq!(
                body.get("source").and_then(Value::as_str),
                Some("rollout"),
                "{leaf} missing source marker"
            );
        }

        // The synthetic context records are genuinely dropped (None), not projected.
        let dev = r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"x"}]}}"#;
        let env = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"x"}]}}"#;
        let tc = r#"{"type":"turn_context","payload":{"turn_id":"t1"}}"#;
        let agentmsg = r#"{"type":"event_msg","payload":{"type":"agent_message","message":"dup"}}"#;
        assert!(map_line(dev).is_none(), "developer message must be dropped");
        assert!(
            map_line(env).is_none(),
            "env/user response_item must be dropped"
        );
        assert!(map_line(tc).is_none(), "turn_context must be dropped");
        assert!(
            map_line(agentmsg).is_none(),
            "agent_message mirror must be dropped"
        );

        // Spot-check field projection: the user prompt, the tool call, its result.
        let user = projected.iter().find(|(l, _)| l == "user/message").unwrap();
        assert_eq!(
            user.1.get("text").and_then(Value::as_str),
            Some("List the files in this directory.")
        );
        let call = projected
            .iter()
            .find(|(l, _)| l == "tool/exec_command/call")
            .unwrap();
        assert_eq!(
            call.1.get("call_id").and_then(Value::as_str),
            Some("call_ABC")
        );
        assert_eq!(
            call.1.get("tool").and_then(Value::as_str),
            Some("exec_command")
        );
        let result = projected.iter().find(|(l, _)| l == "tool/result").unwrap();
        assert_eq!(
            result.1.get("call_id").and_then(Value::as_str),
            Some("call_ABC")
        );
        // Reasoning is redacted (only encrypted_content in the rollout).
        let reasoning = projected
            .iter()
            .find(|(l, _)| l == "assistant/reasoning")
            .unwrap();
        assert_eq!(
            reasoning.1.get("redacted").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn rollout_summary_takes_the_final_agent_message() {
        let mut summary = CaptureSummary::default();
        for line in ROLLOUT_FIXTURE.lines().filter(|l| !l.trim().is_empty()) {
            let rec: Value = serde_json::from_str(line).unwrap();
            rollout_collect_summary(&rec, &mut summary);
        }
        // task_complete's last_agent_message is the authoritative final word.
        assert_eq!(
            summary.final_text.as_deref(),
            Some("There are two entries: Cargo.toml and src.")
        );
    }

    #[test]
    fn rollout_thread_id_and_thread_resolver() {
        // Write the fixture into a temp `<dir>/2026/06/17/rollout-<ts>-<id>.jsonl`
        // and confirm both the header reader and the thread-id resolver find it.
        let thread = "019ed361-4fd2-7793-9ea6-a1add8ca3d4f";
        let base = std::env::temp_dir().join(format!(
            "elanus-rollout-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let day = base.join("2026").join("06").join("17");
        std::fs::create_dir_all(&day).unwrap();
        let file = day.join(format!("rollout-2026-06-17T02-20-30-{thread}.jsonl"));
        std::fs::write(&file, ROLLOUT_FIXTURE).unwrap();
        // Decoy with a different thread id should NOT match.
        let decoy =
            day.join("rollout-2026-06-17T01-00-00-0000ffff-0000-0000-0000-000000000000.jsonl");
        std::fs::write(&decoy, ROLLOUT_FIXTURE).unwrap();

        assert_eq!(rollout_thread_id(&file).as_deref(), Some(thread));
        let found = find_rollout_by_thread(&base, thread).unwrap();
        assert_eq!(found, file);
        assert!(find_rollout_by_thread(&base, "no-such-thread").is_none());

        let _ = std::fs::remove_dir_all(&base);
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
        let (leaf, body) = map_hook_event(claude_agent_noun(), "PreToolUse", &payload);
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
        let (leaf, body) = map_hook_event(claude_agent_noun(), "PostToolUseFailure", &payload);
        assert_eq!(leaf, "tool/Write/result");
        assert_eq!(body["failed"], true);
        assert!(body["response"]
            .as_str()
            .unwrap()
            .contains("permission denied"));
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
            format!(
                "obs/fs/{}",
                topic::encode_path(Path::new("/tmp/proj/notes.md"))
            )
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
    fn read_camera_off_suppresses_m1_projection() {
        // M3's "off is a real state": when sandbox.read_camera = false, the M1
        // read-event projection is GATED OFF at the call site (read_camera_enabled),
        // so a Read tool call publishes NO obs/fs read event — a consumer can't
        // misread silence as "no reads happened" because the broker also fast-fails
        // the read-flavor subscribe (asserted in broker.rs).
        let dir = std::env::temp_dir().join(format!("elanus-rcam-{}", uuid::Uuid::new_v4()));
        let root = Root { dir: dir.clone() };
        std::fs::create_dir_all(root.profile_dir("default")).unwrap();

        // Default (no toggle written) ⇒ ON: the projection runs.
        assert!(
            read_camera_enabled(&root),
            "absent toggle ⇒ camera ON (default)"
        );

        // Explicitly OFF ⇒ the gate is closed.
        std::fs::write(
            root.profile_dir("default").join("profile.toml"),
            "[sandbox]\nread_camera = false\n",
        )
        .unwrap();
        assert!(
            !read_camera_enabled(&root),
            "read_camera=false ⇒ camera OFF"
        );

        // The payload still PROJECTS a read locus in isolation — the suppression is
        // the call-site gate, not the projector — so the off-state is a single,
        // legible switch (read_camera_enabled) rather than scattered logic.
        let payload = json!({
            "cwd": "/tmp/proj",
            "tool_name": "Read",
            "tool_use_id": "toolu_OFF",
            "tool_input": { "file_path": "/tmp/proj/x.md" },
        });
        assert_eq!(claude_read_fs_events(&payload, "s").len(), 1);
        // But with the gate closed, the hook handler would publish none: the gate is
        // `if … && read_camera_enabled(root)`, here proven false.
        assert!(!read_camera_enabled(&root));

        // Flip it back ON and confirm the gate reopens.
        std::fs::write(
            root.profile_dir("default").join("profile.toml"),
            "[sandbox]\nread_camera = true\n",
        )
        .unwrap();
        assert!(read_camera_enabled(&root), "read_camera=true ⇒ camera ON");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn map_user_prompt_and_stop() {
        let (leaf, body) = map_hook_event(
            claude_agent_noun(),
            "UserPromptSubmit",
            &json!({ "prompt": "fix the bug", "session_id": "cc" }),
        );
        assert_eq!(leaf, "user/message");
        assert_eq!(body["prompt"], "fix the bug");

        let (leaf, _) = map_hook_event(claude_agent_noun(), "Stop", &json!({ "session_id": "cc" }));
        assert_eq!(leaf, "session/idle");
    }

    #[test]
    fn unknown_event_still_lands() {
        let (leaf, body) = map_hook_event(claude_agent_noun(), "PreCompact", &json!({ "session_id": "cc" }));
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
        assert!(call_body["command"]
            .as_str()
            .unwrap()
            .contains("echo hello"));

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
        let token = codesession::mint(
            &root,
            principal,
            "claude-code",
            std::process::id() as i32,
            None,
            codesession::RequestedGrants::default(),
        )
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
            room: None,
        };
        let (prog, args) = resume_command_for(&rec, "say hi again");
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
        let (prog, args) = resume_command_for(&rec, "carry on");
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
    fn publish_obs_from_within_a_runtime_does_not_panic() {
        // Regression: the opencode SSE subscriber projects live events from INSIDE its
        // own `block_on` loop. publish_obs → buscli::publish builds a current-thread
        // runtime and `block_on`s it; nesting that inside an existing runtime used to
        // panic ("Cannot start a runtime from within a runtime"). It must now offload to
        // a fresh thread and fail-soft (no daemon here) rather than abort the process.
        let root = Root {
            dir: PathBuf::from("/tmp/elanus-test-no-such-root"),
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            publish_obs(
                &root,
                "owner",
                "tok",
                "obs/agent/x/y/session/idle",
                json!({ "ok": true }),
            );
        });
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

        // A file-writing tool reports its path. REAL opencode 1.17.9 keys the changed
        // file under `state.input.filePath` (this is a trimmed line captured from an
        // actual `opencode run --format json` write tool event — Bug2). If someone
        // reverts the extractor to the old inferred `path` key, this MUST fail.
        opencode_collect_summary(
            &json!({
                "type": "tool_use",
                "sessionID": "ses_real",
                "part": {
                    "type": "tool",
                    "tool": "write",
                    "callID": "call_55fb8c75401547ee94fd5223",
                    "state": { "status": "completed", "input": { "filePath": "/tmp/x.rs", "content": "x" } }
                }
            }),
            &mut summary,
        );
        assert_eq!(summary.file_changes, vec!["/tmp/x.rs"]);
        // The REAL `edit` tool event likewise keys its target under `filePath`.
        opencode_collect_summary(
            &json!({
                "type": "tool_use",
                "sessionID": "ses_real",
                "part": {
                    "type": "tool",
                    "tool": "edit",
                    "callID": "call_35aad0e169ba4e55bbdce02d",
                    "state": { "status": "completed",
                               "input": { "filePath": "/tmp/y.rs", "oldString": "a", "newString": "b" } }
                }
            }),
            &mut summary,
        );
        assert_eq!(summary.file_changes, vec!["/tmp/x.rs", "/tmp/y.rs"]);
        // Legacy fallback: an older binary keying the file under `path` still harvests.
        opencode_collect_summary(
            &json!({
                "type": "tool_use",
                "part": {
                    "tool": "write",
                    "state": { "status": "completed", "input": { "path": "/tmp/z.rs" } }
                }
            }),
            &mut summary,
        );
        assert_eq!(
            summary.file_changes,
            vec!["/tmp/x.rs", "/tmp/y.rs", "/tmp/z.rs"]
        );

        // A non-writing tool does not add a change.
        opencode_collect_summary(
            &json!({
                "type": "tool_use",
                "part": { "tool": "bash", "state": { "status": "completed", "input": {} } }
            }),
            &mut summary,
        );
        assert_eq!(
            summary.file_changes,
            vec!["/tmp/x.rs", "/tmp/y.rs", "/tmp/z.rs"]
        );
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
    fn opencode_serve_url_is_extracted_from_the_announce_line() {
        // The exact line `opencode serve` prints on stdout (v1.17.9, captured live).
        assert_eq!(
            opencode_extract_url("opencode server listening on http://127.0.0.1:4096\n"),
            Some("http://127.0.0.1:4096".to_string())
        );
        // A trailing slash is trimmed (so `{url}/event` is well-formed).
        assert_eq!(
            opencode_extract_url("listening on http://127.0.0.1:51234/"),
            Some("http://127.0.0.1:51234".to_string())
        );
        // A line with no URL yields None.
        assert_eq!(opencode_extract_url("warming up..."), None);
    }

    #[test]
    fn sse_frames_split_on_the_blank_line_and_parse_data_json() {
        // Two complete SSE frames (`data: <json>\n\n`) plus a trailing partial.
        let stream = b"data: {\"type\":\"server.connected\",\"properties\":{}}\n\ndata: {\"type\":\"session.idle\",\"properties\":{\"sessionID\":\"ses_a\"}}\n\ndata: {\"type\":\"partial\"";
        let mut buf = stream.to_vec();
        let mut frames = Vec::new();
        while let Some(pos) = find_frame_end(&buf) {
            let frame: Vec<u8> = buf.drain(..pos).collect();
            buf.drain(..2); // the `\n\n` terminator
            if let Some(ev) = parse_sse_frame(&frame) {
                frames.push(ev);
            }
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["type"], "server.connected");
        assert_eq!(frames[1]["type"], "session.idle");
        // The trailing partial frame stays buffered (no complete frame yet).
        assert!(find_frame_end(&buf).is_none());
        // An optional single leading space after `data:` is tolerated.
        assert_eq!(
            parse_sse_frame(b"data:{\"type\":\"x\"}").unwrap()["type"],
            "x"
        );
    }

    #[test]
    fn sse_message_part_text_translates_to_the_headless_run_event() {
        // A captured-live SSE `message.part.updated` with a SETTLED text part
        // (time.end set) translates into the same `{type:"text", part}` envelope the
        // headless mapper consumes, then reuses opencode_map_event verbatim.
        let sse = json!({
            "id": "evt_1",
            "type": "message.part.updated",
            "properties": {
                "sessionID": "ses_a",
                "part": {
                    "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_a",
                    "type": "text", "text": "Hi! 👋",
                    "time": { "start": 1, "end": 2 }
                }
            }
        });
        let run = opencode_sse_to_run_event(&sse).expect("settled text translates");
        assert_eq!(run["type"], "text");
        // Reusing the headless projection lands the SAME obs leaf as the OC1 path.
        let evs = opencode_map_event(&run);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].0, "assistant/message");
        assert_eq!(evs[0].1["text"], "Hi! 👋");

        // An UNSETTLED text part (no time.end — a streaming partial) is NOT projected
        // (mirrors the headless stream, which only emits settled parts).
        let partial = json!({
            "type": "message.part.updated",
            "properties": { "part": {
                "type": "text", "text": "Hi", "sessionID": "ses_a",
                "time": { "start": 1 }
            }}
        });
        assert!(opencode_sse_to_run_event(&partial).is_none());
    }

    #[test]
    fn sse_message_part_tool_translates_to_call_and_result() {
        // A settled tool part (state.status completed) translates to `tool_use`, which
        // the headless mapper projects as BOTH tool/<n>/call and tool/<n>/result.
        let sse = json!({
            "type": "message.part.updated",
            "properties": { "part": {
                "type": "tool", "tool": "bash", "callID": "call_1",
                "sessionID": "ses_a",
                "state": { "status": "completed", "input": { "command": "ls" }, "output": "f.txt" }
            }}
        });
        let run = opencode_sse_to_run_event(&sse).expect("settled tool translates");
        assert_eq!(run["type"], "tool_use");
        let evs = opencode_map_event(&run);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].0, "tool/bash/call");
        assert_eq!(evs[1].0, "tool/bash/result");
        assert_eq!(evs[1].1["output"], "f.txt");

        // A tool part still RUNNING (status:"running") is not yet projected.
        let running = json!({
            "type": "message.part.updated",
            "properties": { "part": {
                "type": "tool", "tool": "bash", "callID": "c",
                "state": { "status": "running", "input": {} }
            }}
        });
        assert!(opencode_sse_to_run_event(&running).is_none());

        // reasoning + step parts remap to their headless type values.
        let reasoning = json!({
            "type": "message.part.updated",
            "properties": { "part": {
                "type": "reasoning", "text": "thinking", "sessionID": "ses_a",
                "time": { "start": 1, "end": 2 }
            }}
        });
        assert_eq!(
            opencode_sse_to_run_event(&reasoning).unwrap()["type"],
            "reasoning"
        );
        let step = json!({
            "type": "message.part.updated",
            "properties": { "part": { "type": "step-finish", "tokens": {}, "cost": 0 } }
        });
        assert_eq!(
            opencode_sse_to_run_event(&step).unwrap()["type"],
            "step_finish"
        );

        // A non-content SSE event has no headless analog → None (handled directly).
        let idle = json!({ "type": "session.idle", "properties": { "sessionID": "ses_a" } });
        assert!(opencode_sse_to_run_event(&idle).is_none());
    }

    #[test]
    fn sse_native_session_id_is_harvested_from_the_stream() {
        // `session.created` carries the native id under properties.info.id — the same
        // id the headless `run` stream carries on `sessionID`, so OC2 resume works
        // from a TUI session.
        let created = json!({
            "type": "session.created",
            "properties": { "sessionID": "ses_z", "info": { "id": "ses_z", "slug": "x" } }
        });
        assert_eq!(opencode_sse_native_id(&created).as_deref(), Some("ses_z"));
        // Content events also carry it under properties.sessionID (the fallback).
        let part = json!({
            "type": "message.part.updated",
            "properties": { "sessionID": "ses_y", "part": { "type": "text" } }
        });
        assert_eq!(opencode_sse_native_id(&part).as_deref(), Some("ses_y"));
        // A frame with no session id (server.connected) yields None.
        let conn = json!({ "type": "server.connected", "properties": {} });
        assert_eq!(opencode_sse_native_id(&conn), None);
    }

    #[test]
    fn sse_fidelity_is_stamped_live() {
        // Every projected SSE body is stamped LIVE so a consumer never mistakes it for
        // codex's post-hoc rollout import or a Claude hook bridge.
        let body = stamp_sse_fidelity(json!({ "ts": "t", "text": "x" }));
        assert_eq!(body["fidelity"], "server-events-live");
        assert_eq!(body["source"], "sse");
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
        let (prog, args) = resume_command_for(&rec, "continue please");
        assert_eq!(prog, "claude");
        assert!(args.contains(&"-p".to_string()));
        let resume_pos = args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(args[resume_pos + 1], "cc-sess-9f");
        assert!(args
            .windows(2)
            .any(|w| w == ["--output-format", "stream-json"]));
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
        assert!(delivery_requester(
            &root,
            &json!({ "prompt": "go", "reply_to": "in/agent/+/code-x" }),
            None,
            None,
        )
        .is_none());
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
        assert!(delivery_requester(
            &root,
            &json!({ "prompt": "x", "reply_to": "../../owner" }),
            Some("owner"),
            None,
        )
        .is_none());
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
        // The daemon resume primitive on a session that was never recorded is a
        // clean error, not a panic and not a silent no-op (so the daemon sees the
        // missing record). There is no human `resume` verb; `resume_capture` is the
        // in-process primitive the daemon drives.
        let dir = std::env::temp_dir().join(format!("elanus-resume-norec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = Root { dir: dir.clone() };
        let err = resume_capture(&root, "code-nope0000", "hi").unwrap_err();
        assert!(format!("{err:#}").contains("no resumable coding session"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn interactive_resume_hint_is_per_tool_managed_passthrough() {
        // Claude has a clean passthrough (`--resume <native>`); codex/opencode do
        // not (their launch treats a positional as a seed prompt), so they get no
        // suggestion. An unknown tool or empty native id also yields None.
        assert_eq!(
            interactive_resume_hint("claude", "38472ce9").as_deref(),
            Some("elanus code claude --resume 38472ce9")
        );
        assert_eq!(interactive_resume_hint("codex", "abc"), None);
        assert_eq!(interactive_resume_hint("opencode", "abc"), None);
        assert_eq!(interactive_resume_hint("nonesuch", "abc"), None);
        assert_eq!(interactive_resume_hint("claude", ""), None);
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
        codesession::mint(
            &root,
            principal,
            "codex",
            std::process::id() as i32,
            None,
            codesession::RequestedGrants::default(),
        )
        .unwrap();

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
        // It teaches the two-axis model (launch mode vs drive pattern) crisply:
        // names the --headless flag, bare → TUI, and the live/async split.
        assert!(b.contains("--headless"));
        assert!(b.to_lowercase().contains("tui"));
        let bl = b.to_lowercase();
        assert!(bl.contains("launch mode") && bl.contains("drive pattern"));
        // Short — a launch briefing, not a manual. (Teaching both axes costs a
        // little over the old single-axis text; the cap stays tight.)
        assert!(
            b.len() < 1300,
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
    fn elanus_skill_plugin_is_session_scratch_scoped() {
        let dir = std::env::temp_dir().join(format!(
            "elanus-skill-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // A source skill package to materialize alongside the bootstrap skill.
        let pkg = dir.join("pkgs/wiring-probe");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("SKILL.md"), "probe body").unwrap();
        let skills = vec![("wiring-probe".to_string(), pkg.clone())];

        let plugin = build_claude_skill_plugin(&dir, &skills).unwrap();
        assert_eq!(plugin, dir.join("plugin"));

        // The plugin manifest makes `--plugin-dir` recognize it.
        let manifest =
            std::fs::read_to_string(plugin.join(".claude-plugin/plugin.json")).unwrap();
        assert!(manifest.contains("\"name\":\"elanus\""));

        // The bootstrap `/elanus` skill is a real file under skills/elanus.
        let boot = std::fs::read_to_string(plugin.join("skills/elanus/SKILL.md")).unwrap();
        assert!(boot.contains("name: elanus"));
        assert!(boot.contains("elanus code help"));
        assert!(boot.contains("elanus code claude --headless"));

        // The profile skill is SYMLINKED alongside it (live, not copied) and its
        // SKILL.md is reachable through the link.
        let link = plugin.join("skills/wiring-probe");
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), pkg);
        assert!(link.join("SKILL.md").exists());

        // The generated settings.json never leaks into the plugin dir.
        assert!(!plugin.join("settings.json").exists());

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
        // C2 (agent-comms): the inbox is now the computed `inbox` block.
        assert!(one.starts_with("[elanus block: inbox]"));
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
        assert!(turn_injection(&root, "codex", "code-uns00001")
            .unwrap()
            .contains("2 new message"));
        // Pulling marks the first seen → the next turn reflects only the unseen.
        codesession::mark_inbox_seen(&root, "code-uns00001", &[id1]).unwrap();
        assert!(turn_injection(&root, "codex", "code-uns00001")
            .unwrap()
            .contains("1 new message"));
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
        // C2 (agent-comms): with only a note (empty inbox), the injection leads with
        // the note line; the inbox text is now a block that only appears when mail
        // is waiting. The point stands: the injection is prepended.
        assert!(injected.starts_with("[elanus note]"));
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

    // ── M4: take_grants_flags unit tests ─────────────────────────────────────

    fn s(v: &str) -> String { v.to_string() }
    fn sv(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    // ── M2: take_provider_flag + injection-application unit tests ─────────────

    #[test]
    fn take_provider_flag_absent_leaves_argv_unchanged() {
        // The no-`--provider` invariant: argv byte-identical, provider None.
        let (p, rest) = take_provider_flag(&sv(&["claude", "--resume", "fix it"])).unwrap();
        assert_eq!(p, None);
        assert_eq!(rest, sv(&["claude", "--resume", "fix it"]));
    }

    #[test]
    fn take_provider_flag_strips_before_tool_token() {
        // `elanus code --provider deepseek claude --resume` → (deepseek, claude --resume).
        let (p, rest) =
            take_provider_flag(&sv(&["--provider", "deepseek", "claude", "--resume"])).unwrap();
        assert_eq!(p.as_deref(), Some("deepseek"));
        assert_eq!(rest, sv(&["claude", "--resume"]));
    }

    #[test]
    fn take_provider_flag_leaves_tool_args_verbatim() {
        // Everything after the tool token forwards verbatim — including a token that
        // happens to look like our flag (it's the tool's arg now, not ours).
        let (p, rest) = take_provider_flag(&sv(&[
            "--provider", "ds", "codex", "--provider", "not-ours", "do it",
        ]))
        .unwrap();
        assert_eq!(p.as_deref(), Some("ds"));
        assert_eq!(rest, sv(&["codex", "--provider", "not-ours", "do it"]));
    }

    #[test]
    fn take_provider_flag_after_tool_token_is_a_tool_arg() {
        // A `--provider` that only appears AFTER the tool token is NOT consumed.
        let (p, rest) = take_provider_flag(&sv(&["claude", "--provider", "x"])).unwrap();
        assert_eq!(p, None);
        assert_eq!(rest, sv(&["claude", "--provider", "x"]));
    }

    #[test]
    fn take_provider_flag_missing_value_errors() {
        let err = take_provider_flag(&sv(&["--provider"])).unwrap_err().to_string();
        assert!(err.contains("requires a value"), "{err}");
        // A flag as the value is also a usage error (would swallow the next flag).
        let err = take_provider_flag(&sv(&["--provider", "--headless", "claude"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires a value"), "{err}");
    }

    #[test]
    fn take_provider_flag_duplicate_errors() {
        let err = take_provider_flag(&sv(&["--provider", "a", "--provider", "b", "claude"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("at most once"), "{err}");
    }

    #[test]
    fn apply_provider_injection_seam_codex_env_args_no_secret() {
        // The injection-application seam: materialize a codex ApiKey provider, then
        // apply it to a Command. Assert (a) the -c args select the custom provider
        // with a QUOTED hyphenated id, (b) the secret rides env (never the args),
        // (c) the env_key pair lands on the child, and (d) inherited harness-config
        // vars (a parent's provider) are scrubbed so they can't bleed through.
        use crate::provider::{
            materialize, Consumer, Credential, HarnessId, Injection, Secret, Wire,
        };
        let cred = Credential::ApiKey {
            wire: Wire::OpenAI,
            base_url: "https://api.example.com".to_string(),
            key: Secret::new("sk-secret-xyz"),
            headers: vec![],
        };
        let Injection::Harness(inj) = materialize(
            "deepseek-anthropic",
            &cred,
            Consumer::Harness(HarnessId::Codex),
            None,
        )
        .unwrap() else {
            panic!("codex consumer yields a harness injection")
        };
        let joined = inj.args.join(" ");
        assert!(
            joined.contains("model_provider=\"deepseek-anthropic\""),
            "hyphenated id must be a quoted TOML value: {joined}"
        );
        assert!(
            !joined.contains("sk-secret-xyz"),
            "the secret must NEVER appear in a -c arg"
        );

        let mut cmd = std::process::Command::new("true");
        // A parent's leaked harness config that must NOT survive into the child.
        cmd.env("CODEX_HOME", "/parent/.codex");
        cmd.env("OPENCODE_CONFIG_CONTENT", "{parent}");
        apply_provider_injection_env(&mut cmd, &inj);
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|x| x.to_string_lossy().into_owned()),
                )
            })
            .collect();
        // The env_key pair carries the secret to the child (this is where it lives).
        assert!(
            envs.iter().any(|(k, v)| k == "ELANUS_PV_DEEPSEEK_ANTHROPIC_KEY"
                && v.as_deref() == Some("sk-secret-xyz")),
            "env_key pair must be set: {envs:?}"
        );
        // Inherited harness-config vars are scheduled for removal (None).
        assert!(
            envs.iter().any(|(k, v)| k == "CODEX_HOME" && v.is_none()),
            "inherited CODEX_HOME must be scrubbed: {envs:?}"
        );
        assert!(
            envs.iter()
                .any(|(k, v)| k == "OPENCODE_CONFIG_CONTENT" && v.is_none()),
            "inherited OPENCODE_CONFIG_CONTENT must be scrubbed: {envs:?}"
        );
    }

    #[test]
    fn apply_provider_injection_native_login_is_clean_child_scrub() {
        // NativeLogin yields an empty injection, but applying it still scrubs the
        // harness-config vars so an explicit native-login child is clean (the
        // nesting guarantee for the "named default").
        use crate::provider::{
            materialize, Consumer, Credential, HarnessId, Injection,
        };
        let Injection::Harness(inj) = materialize(
            "native",
            &Credential::NativeLogin { tool: None },
            Consumer::Harness(HarnessId::Codex),
            None,
        )
        .unwrap() else {
            panic!()
        };
        assert!(inj.env.is_empty() && inj.args.is_empty());
        let mut cmd = std::process::Command::new("true");
        cmd.env("CODEX_HOME", "/parent/.codex");
        apply_provider_injection_env(&mut cmd, &inj);
        let scrubbed = cmd
            .get_envs()
            .any(|(k, v)| k.to_string_lossy() == "CODEX_HOME" && v.is_none());
        assert!(scrubbed, "native-login apply must still scrub harness-config vars");
    }

    #[test]
    fn take_grants_flags_absent_gives_default() {
        // No M4 flags → RequestedGrants::default() and args unchanged.
        let (grants, rest) = take_grants_flags(&sv(&["claude", "fix it"])).unwrap();
        assert!(grants.budget.is_none());
        assert!(grants.publish.is_none());
        assert!(grants.subscribe.is_none());
        assert!(grants.fs_write.is_none());
        assert!(grants.fs_read.is_none());
        assert!(grants.tool_allowlist.is_none());
        assert!(grants.blocking.is_none());
        assert_eq!(rest, sv(&["claude", "fix it"]));
    }

    #[test]
    fn take_grants_flags_budget_parsed() {
        let (grants, rest) = take_grants_flags(&sv(&["--budget", "42", "do it"])).unwrap();
        assert_eq!(grants.budget, Some(42));
        assert_eq!(rest, sv(&["do it"]));
    }

    #[test]
    fn take_grants_flags_budget_zero_ok() {
        let (grants, _) = take_grants_flags(&sv(&["--budget", "0"])).unwrap();
        assert_eq!(grants.budget, Some(0));
    }

    #[test]
    fn take_grants_flags_budget_non_numeric_errors() {
        let err = take_grants_flags(&sv(&["--budget", "abc"])).unwrap_err();
        assert!(err.to_string().contains("--budget"), "msg: {err}");
    }

    #[test]
    fn take_grants_flags_budget_missing_value_errors() {
        let err = take_grants_flags(&sv(&["--budget"])).unwrap_err();
        assert!(err.to_string().contains("--budget"), "msg: {err}");
    }

    #[test]
    fn take_grants_flags_publish_accumulates() {
        let (grants, rest) = take_grants_flags(
            &sv(&["--grant-publish", "obs/#", "--grant-publish", "work/+/status", "go"])
        ).unwrap();
        assert_eq!(grants.publish, Some(vec![s("obs/#"), s("work/+/status")]));
        assert_eq!(rest, sv(&["go"]));
    }

    #[test]
    fn take_grants_flags_subscribe_accumulates() {
        let (grants, _) = take_grants_flags(
            &sv(&["--grant-subscribe", "in/agent/#", "--grant-subscribe", "obs/agent/+/code-abc/#"])
        ).unwrap();
        assert_eq!(grants.subscribe, Some(vec![s("in/agent/#"), s("obs/agent/+/code-abc/#")]));
    }

    #[test]
    fn take_grants_flags_publish_invalid_filter_errors() {
        // "#" in the middle of a filter is invalid.
        let err = take_grants_flags(&sv(&["--grant-publish", "obs/#/bad"])).unwrap_err();
        assert!(err.to_string().contains("--grant-publish"), "msg: {err}");
    }

    #[test]
    fn take_grants_flags_subscribe_invalid_filter_errors() {
        let err = take_grants_flags(&sv(&["--grant-subscribe", "##"])).unwrap_err();
        assert!(err.to_string().contains("--grant-subscribe"), "msg: {err}");
    }

    #[test]
    fn take_grants_flags_fs_write_absolute_ok() {
        let (grants, rest) = take_grants_flags(
            &sv(&["--grant-fs-write", "/home/user/proj", "extra"])
        ).unwrap();
        assert_eq!(grants.fs_write, Some(vec![s("/home/user/proj")]));
        assert_eq!(rest, sv(&["extra"]));
    }

    #[test]
    fn take_grants_flags_fs_write_accumulates() {
        let (grants, _) = take_grants_flags(
            &sv(&["--grant-fs-write", "/a", "--grant-fs-write", "/b"])
        ).unwrap();
        assert_eq!(grants.fs_write, Some(vec![s("/a"), s("/b")]));
    }

    #[test]
    fn take_grants_flags_fs_write_relative_errors() {
        // Security: a relative path is a root-wildcard footgun — reject at construction.
        let err = take_grants_flags(&sv(&["--grant-fs-write", "relative/path"])).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--grant-fs-write"), "msg: {msg}");
        assert!(msg.contains("absolute"), "msg: {msg}");
    }

    #[test]
    fn take_grants_flags_fs_read_relative_errors() {
        let err = take_grants_flags(&sv(&["--grant-fs-read", "relative/p"])).unwrap_err();
        assert!(err.to_string().contains("absolute"), "msg: {err}");
    }

    #[test]
    fn take_grants_flags_fs_write_empty_errors() {
        // An empty path would also be the root-wildcard footgun (path_covered rejects it).
        // In practice the shell won't pass an empty arg, but validate anyway.
        // A flag whose next token looks like another flag triggers the "no value" error.
        let err = take_grants_flags(&sv(&["--grant-fs-write", "--grant-fs-read", "/ok"])).unwrap_err();
        assert!(err.to_string().contains("--grant-fs-write"), "msg: {err}");
    }

    #[test]
    fn take_grants_flags_fs_write_degenerate_absolute_errors() {
        // is_absolute() alone admits near-root grants — reject them at construction
        // (defense-in-depth, mirroring path_covered's deny-when-degenerate posture).
        for bad in ["/", "//", "/.", "/../..", "/a/..", "/a/.."] {
            let err = take_grants_flags(&sv(&["--grant-fs-write", bad])).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("root") || msg.contains(".."),
                "expected a degenerate-path rejection for {bad:?}, got: {msg}"
            );
        }
        // Leading/trailing whitespace is rejected too.
        let err = take_grants_flags(&sv(&["--grant-fs-write", "/abs "])).unwrap_err();
        assert!(err.to_string().contains("whitespace"), "msg: {err}");
        // A real directory below root is accepted.
        let (g, _) = take_grants_flags(&sv(&["--grant-fs-write", "/home/u/proj"])).unwrap();
        assert_eq!(g.fs_write, Some(vec!["/home/u/proj".to_string()]));
    }

    #[test]
    fn take_grants_flags_fs_read_accumulates() {
        let (grants, _) = take_grants_flags(
            &sv(&["--grant-fs-read", "/read/a", "--grant-fs-read", "/read/b"])
        ).unwrap();
        assert_eq!(grants.fs_read, Some(vec![s("/read/a"), s("/read/b")]));
    }

    #[test]
    fn take_grants_flags_tool_allowlist_accumulates() {
        let (grants, rest) = take_grants_flags(
            &sv(&["--grant-tool", "bash", "--grant-tool", "read_file", "arg"])
        ).unwrap();
        assert_eq!(grants.tool_allowlist, Some(vec![s("bash"), s("read_file")]));
        assert_eq!(rest, sv(&["arg"]));
    }

    #[test]
    fn take_grants_flags_blocking_accumulates() {
        let (grants, _) = take_grants_flags(
            &sv(&["--grant-blocking", "disk-io", "--grant-blocking", "network"])
        ).unwrap();
        assert_eq!(grants.blocking, Some(vec![s("disk-io"), s("network")]));
    }

    #[test]
    fn take_grants_flags_remaining_args_preserved_in_order() {
        // Non-M4 args pass through untouched and in original order.
        let args = sv(&["--budget", "5", "claude", "--model", "opus", "--grant-publish", "obs/#", "task"]);
        let (grants, rest) = take_grants_flags(&args).unwrap();
        assert_eq!(grants.budget, Some(5));
        assert_eq!(grants.publish, Some(vec![s("obs/#")]));
        assert_eq!(rest, sv(&["claude", "--model", "opus", "task"]));
    }

    #[test]
    fn take_grants_flags_all_fields_together() {
        let args = sv(&[
            "--budget", "10",
            "--grant-publish", "obs/#",
            "--grant-subscribe", "in/agent/#",
            "--grant-fs-write", "/tmp/work",
            "--grant-fs-read", "/src",
            "--grant-tool", "grep",
            "--grant-blocking", "shell",
            "the-task",
        ]);
        let (grants, rest) = take_grants_flags(&args).unwrap();
        assert_eq!(grants.budget, Some(10));
        assert_eq!(grants.publish, Some(vec![s("obs/#")]));
        assert_eq!(grants.subscribe, Some(vec![s("in/agent/#")]));
        assert_eq!(grants.fs_write, Some(vec![s("/tmp/work")]));
        assert_eq!(grants.fs_read, Some(vec![s("/src")]));
        assert_eq!(grants.tool_allowlist, Some(vec![s("grep")]));
        assert_eq!(grants.blocking, Some(vec![s("shell")]));
        assert_eq!(rest, sv(&["the-task"]));
    }

    // ── M4: end-to-end mint-layer acceptance tests ───────────────────────────

    #[test]
    fn m4_owner_budget_sets_remaining() {
        // An owner-launched session minted with --budget 4 has remaining = 4.
        let root = m3_tmp_root();
        let pid = std::process::id() as i32;
        let tok = codesession::mint(
            &root, "code-m4owner1", "claude-code", pid, None,
            codesession::RequestedGrants { budget: Some(4), ..Default::default() }
        ).unwrap();
        assert_eq!(tok.grants.turn_budget, Some(4));
        assert_eq!(tok.grants.remaining_budget, Some(4));
    }

    #[test]
    fn m4_child_budget_exceeds_parent_is_refused() {
        // A child requesting budget 10 from a parent with budget 4 is refused.
        let root = m3_tmp_root();
        let pid = std::process::id() as i32;
        codesession::mint(
            &root, "code-m4par2", "claude-code", pid, None,
            codesession::RequestedGrants { budget: Some(4), ..Default::default() }
        ).unwrap();
        let err = codesession::mint(
            &root, "code-m4child2", "claude-code", pid,
            Some("code-m4par2"),
            codesession::RequestedGrants { budget: Some(10), ..Default::default() }
        ).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("budget") || msg.contains("remaining") || msg.contains("exceed"),
            "expected budget-exceeded error, got: {msg}"
        );
    }

    #[test]
    fn m4_child_fs_write_outside_parent_refused() {
        // A child requesting an fs_write outside the owner's grant is refused.
        let root = m3_tmp_root();
        let pid = std::process::id() as i32;
        // Owner has fs_write limited to /tmp/work.
        codesession::mint(
            &root, "code-m4fspar", "claude-code", pid, None,
            codesession::RequestedGrants {
                fs_write: Some(vec!["/tmp/work".to_string()]),
                ..Default::default()
            }
        ).unwrap();
        // Child tries to widen to /tmp (a broader prefix that would cover /tmp/work
        // and more) — refused because /tmp is not covered by /tmp/work.
        let err = codesession::mint(
            &root, "code-m4fschild", "claude-code", pid,
            Some("code-m4fspar"),
            codesession::RequestedGrants {
                fs_write: Some(vec!["/tmp".to_string()]),
                ..Default::default()
            }
        ).unwrap_err();
        assert!(
            err.to_string().contains("fs_write") || err.to_string().contains("not covered"),
            "expected fs_write widening error, got: {err}"
        );
    }

    #[test]
    fn m4_child_publish_outside_parent_refused() {
        // A child requesting a publish filter not covered by the parent is refused.
        let root = m3_tmp_root();
        let pid = std::process::id() as i32;
        // Owner minted without explicit publish → unbounded (None).
        // Then that session spawns with publish limited to obs/agent/claude-code/code-m4pubpar/#.
        // Owner mint → publish defaults to the parent's own structural subtree.
        let own_pub = "obs/agent/claude-code/code-m4pubpar/#".to_string();
        codesession::mint(
            &root, "code-m4pubpar", "claude-code", pid, None,
            codesession::RequestedGrants { ..Default::default() }
        ).unwrap();
        // Setup (asserted unconditionally so the refusal check below can never be
        // skipped vacuously): a child requesting exactly the spawner's own publish
        // filter is covered by the spawner → granted.
        let child_ok = codesession::mint(
            &root, "code-m4pubchok", "claude-code", pid,
            Some("code-m4pubpar"),
            codesession::RequestedGrants {
                publish: Some(vec![own_pub.clone()]),
                ..Default::default()
            }
        );
        assert!(
            child_ok.is_ok(),
            "setup: a child requesting the spawner's own publish filter must be granted: {child_ok:?}"
        );
        // The real check: a child requesting obs/# (wider than the parent's narrow
        // scope, and not its own subtree) must be REFUSED.
        let err = codesession::mint(
            &root, "code-m4pubchfail", "claude-code", pid,
            Some("code-m4pubpar"),
            codesession::RequestedGrants {
                publish: Some(vec!["obs/#".to_string()]),
                ..Default::default()
            }
        );
        assert!(
            err.is_err(),
            "child requesting obs/# from a narrowly-scoped parent must be refused"
        );
        // And take_grants_flags parsed the wide filter without itself bounding it
        // (well-formedness only; mint does the bounding).
        let (grants, _) = take_grants_flags(&sv(&["--grant-publish", "obs/#"])).unwrap();
        assert_eq!(grants.publish, Some(vec!["obs/#".to_string()]));
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
        assert!(
            r1.starts_with("wd-"),
            "default room is workdir-derived: {r1}"
        );
        assert_eq!(
            r1, r2,
            "the same workdir must derive the SAME room (stable)"
        );
        // A different dir derives a different room.
        let other = resolve_room(None, Path::new("/"));
        assert_ne!(r1, other, "distinct workdirs derive distinct rooms");
    }

    #[test]
    fn sa1_explicit_room_overrides_workdir_default() {
        let dir = std::env::temp_dir();
        let explicit = resolve_room(Some("team-1"), &dir);
        assert_eq!(
            explicit, "team-1",
            "an explicit --room wins over the workdir"
        );
        // A blank/whitespace explicit value falls back to the workdir default.
        let blank = resolve_room(Some("   "), &dir);
        assert!(
            blank.starts_with("wd-"),
            "a blank --room falls back to workdir: {blank}"
        );
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
        // C2 (agent-comms): the inbox now renders as the `inbox` block.
        assert!(
            inj.starts_with("[elanus block: inbox]"),
            "solo block is unchanged: {inj}"
        );
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

    // ── SA3 (write half): touching a file IS the claim ───────────────────────

    /// Record a session in `root`'s ledger with a workdir-derived (default) room,
    /// returning its (session, room) so a test can assert what a roommate sees.
    fn sa3_record_session(root: &Root, session: &str, workdir: &str) -> String {
        codesession::upsert_record(
            root,
            &codesession::SessionRecord {
                elanus_session: session.into(),
                native_session: format!("native-{session}"),
                tool: "claude".into(),
                agent_noun: claude_agent_noun().into(),
                workdir: workdir.into(),
                room: None,
            },
        )
        .unwrap();
        // Mirror the resolution auto_claim_write uses (no explicit room → workdir).
        resolve_room(None, Path::new(workdir))
    }

    #[test]
    fn sa3_write_auto_claims_in_session_room_and_a_peer_sees_it() {
        let root = delivery_tmp_root();
        let workdir = "/tmp/sa3-proj";
        let room = sa3_record_session(&root, "code-aaaa1111", workdir);

        // Agent A writes a file via a tool — NO `elanus code claim` is ever run.
        auto_claim_write(&root, "code-aaaa1111", "src/foo.rs", Some(workdir));

        // A roommate B in the SAME workdir-derived room sees A's claim as a peer,
        // for the canonical path (acceptance: "appears, for that path, in a
        // roommate's claims").
        let want = std::fs::canonicalize(Path::new(workdir).join("src/foo.rs"))
            .unwrap_or_else(|_| Path::new(workdir).join("src/foo.rs"))
            .to_string_lossy()
            .to_string();
        let peers = codesession::peer_claims(&root, &room, "code-bbbb2222").unwrap();
        assert!(
            peers
                .iter()
                .any(|c| c.session == "code-aaaa1111" && c.path == want),
            "roommate must see A's auto-claim on {want}; saw {peers:?}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn sa3_re_editing_the_same_path_does_not_duplicate() {
        let root = delivery_tmp_root();
        let workdir = "/tmp/sa3-dedupe";
        let room = sa3_record_session(&root, "code-cccc3333", workdir);

        // Same file written three times (the common re-edit case) → ONE claim.
        for _ in 0..3 {
            auto_claim_write(&root, "code-cccc3333", "lib.rs", Some(workdir));
        }
        let peers = codesession::peer_claims(&root, &room, "code-dddd4444").unwrap();
        assert_eq!(
            peers
                .iter()
                .filter(|c| c.session == "code-cccc3333")
                .count(),
            1,
            "re-editing the same path must not spam claims (idempotent upsert)"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn sa3_blank_path_is_a_noop() {
        let root = delivery_tmp_root();
        let workdir = "/tmp/sa3-blank";
        let room = sa3_record_session(&root, "code-eeee5555", workdir);

        // A blank/whitespace path (a malformed tool event) records nothing and
        // never panics.
        auto_claim_write(&root, "code-eeee5555", "", Some(workdir));
        auto_claim_write(&root, "code-eeee5555", "   ", Some(workdir));
        let peers = codesession::peer_claims(&root, &room, "code-ffff6666").unwrap();
        assert!(
            peers.is_empty(),
            "a blank path must claim nothing; saw {peers:?}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn sa3_no_record_yet_is_a_noop() {
        // Before the native session id is observed there is no durable record, so
        // the room can't be resolved — auto-claim skips silently (no panic).
        let root = delivery_tmp_root();
        auto_claim_write(&root, "code-99999999", "src/x.rs", Some("/tmp/whatever"));
        // Nothing recorded under any room (the workdir-room of the would-be path).
        let room = resolve_room(None, Path::new("/tmp/whatever"));
        let peers = codesession::peer_claims(&root, &room, "code-other000").unwrap();
        assert!(peers.is_empty());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn sa3_explicit_room_on_record_wins() {
        // A session launched with `--room` has that room on its record; the
        // auto-claim must land THERE (not the workdir-room), matching the CLI.
        let root = delivery_tmp_root();
        codesession::upsert_record(
            &root,
            &codesession::SessionRecord {
                elanus_session: "code-room1111".into(),
                native_session: "native-room".into(),
                tool: "claude".into(),
                agent_noun: claude_agent_noun().into(),
                workdir: "/tmp/sa3-explicit".into(),
                room: Some("planner-room".into()),
            },
        )
        .unwrap();
        auto_claim_write(&root, "code-room1111", "/tmp/sa3-explicit/a.rs", None);
        let peers = codesession::peer_claims(&root, "planner-room", "code-peer0000").unwrap();
        assert_eq!(peers.len(), 1, "auto-claim must land in the explicit room");
        // And NOT in the workdir-room.
        let wd_room = resolve_room(None, Path::new("/tmp/sa3-explicit"));
        assert!(codesession::peer_claims(&root, &wd_room, "code-peer0000")
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn sa3_resumed_session_relative_path_resolves_against_recorded_workdir() {
        // BUG A: on a RESUMED codex/opencode session the capture is called with
        // record_workdir=None → auto_claim_write gets cwd=None. A RELATIVE harness
        // path must resolve against the session's RECORDED workdir (what launch used)
        // — NOT the launcher process cwd — so resume claims the identical absolute
        // path launch would. Use a real temp dir + file so canonicalize succeeds and
        // the assertion is independent of the test process's own cwd.
        let root = delivery_tmp_root();
        let workdir = root.dir.join("resumed-proj");
        std::fs::create_dir_all(workdir.join("src")).unwrap();
        std::fs::write(workdir.join("src/foo.rs"), b"// x").unwrap();
        let workdir = std::fs::canonicalize(&workdir).unwrap();
        let workdir = workdir.to_string_lossy().to_string();
        let room = sa3_record_session(&root, "code-resume01", &workdir);

        // Resume: cwd=None, a RELATIVE path from the harness.
        auto_claim_write(&root, "code-resume01", "src/foo.rs", None);

        let want = std::fs::canonicalize(Path::new(&workdir).join("src/foo.rs"))
            .unwrap()
            .to_string_lossy()
            .to_string();
        // Sanity: the recorded workdir is NOT the test process cwd, so a process-cwd
        // resolution would have produced a different (wrong) path.
        assert_ne!(
            std::env::current_dir().unwrap().to_string_lossy(),
            workdir,
            "test precondition: recorded workdir must differ from process cwd"
        );
        let peers = codesession::peer_claims(&root, &room, "code-resviewer").unwrap();
        assert!(
            peers
                .iter()
                .any(|c| c.session == "code-resume01" && c.path == want),
            "resumed (cwd=None) relative path must claim {want} (resolved against the \
             recorded workdir); saw {peers:?}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn sa3_manual_and_auto_claim_of_one_file_collapse_to_one_row() {
        // BUG B: a manual `claim src/foo.rs` (was stored VERBATIM) and the SA3
        // auto-claim of the same file (stored CANONICAL) used to land as TWO rows,
        // double-listing one file for a roommate. Both now canonicalize against the
        // session workdir, so they collapse to ONE row per (room, session, path).
        let root = delivery_tmp_root();
        let workdir = root.dir.join("collapse-proj");
        std::fs::create_dir_all(workdir.join("src")).unwrap();
        std::fs::write(workdir.join("src/foo.rs"), b"// x").unwrap();
        let workdir = std::fs::canonicalize(&workdir).unwrap();
        let workdir = workdir.to_string_lossy().to_string();
        let room = sa3_record_session(&root, "code-collapse1", &workdir);

        // Manual claim path: claim_cmd canonicalizes a relative path against the
        // recorded workdir before add_claim — exercise that exact shared resolution.
        let manual = canonicalize_claim_path("src/foo.rs", Some(&workdir)).unwrap();
        codesession::add_claim(&root, &room, "code-collapse1", &manual).unwrap();
        // Auto-claim of the SAME file (relative, resolved against workdir).
        auto_claim_write(&root, "code-collapse1", "src/foo.rs", Some(&workdir));

        let peers = codesession::peer_claims(&root, &room, "code-collapsevw").unwrap();
        let n = peers
            .iter()
            .filter(|c| c.session == "code-collapse1")
            .count();
        assert_eq!(
            n, 1,
            "manual + auto claim of one file must be ONE row, not two; saw {peers:?}"
        );
        let want = std::fs::canonicalize(Path::new(&workdir).join("src/foo.rs"))
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(
            peers[0].path, want,
            "the one row must be the canonical path"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn sa3_blank_and_no_record_paths_stay_safe_noops() {
        // Guardrail (unchanged): blank/whitespace paths and a missing record both
        // record nothing and never panic — on BOTH the cwd=Some and cwd=None paths.
        let root = delivery_tmp_root();
        let workdir = "/tmp/sa3-safe";
        let room = sa3_record_session(&root, "code-safe0001", workdir);
        auto_claim_write(&root, "code-safe0001", "", None);
        auto_claim_write(&root, "code-safe0001", "   ", Some(workdir));
        assert!(canonicalize_claim_path("   ", Some(workdir)).is_none());
        // No record at all (native id not observed) is a no-op even with cwd=None.
        auto_claim_write(&root, "code-norecord0", "src/x.rs", None);
        let peers = codesession::peer_claims(&root, &room, "code-safeview0").unwrap();
        assert!(
            peers.is_empty(),
            "blank/no-record must claim nothing; saw {peers:?}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn sa3_claude_write_tool_path_detects_the_write_tools() {
        // The PreToolUse write signals: Write/Edit/MultiEdit carry file_path;
        // NotebookEdit carries notebook_path; read/other tools are NOT writes.
        let wr = json!({ "file_path": "/x/a.rs" });
        assert_eq!(claude_write_tool_path("Write", Some(&wr)), Some("/x/a.rs"));
        assert_eq!(claude_write_tool_path("Edit", Some(&wr)), Some("/x/a.rs"));
        assert_eq!(
            claude_write_tool_path("MultiEdit", Some(&wr)),
            Some("/x/a.rs")
        );
        let nb = json!({ "notebook_path": "/x/n.ipynb" });
        assert_eq!(
            claude_write_tool_path("NotebookEdit", Some(&nb)),
            Some("/x/n.ipynb")
        );
        // Read/Grep/Bash are not write signals (the read half is deferred).
        assert_eq!(claude_write_tool_path("Read", Some(&wr)), None);
        assert_eq!(claude_write_tool_path("Bash", Some(&wr)), None);
        // A blank or missing path is no write.
        assert_eq!(
            claude_write_tool_path("Write", Some(&json!({ "file_path": "" }))),
            None
        );
        assert_eq!(claude_write_tool_path("Write", Some(&json!({}))), None);
        assert_eq!(claude_write_tool_path("Write", None), None);
    }

    #[test]
    fn sa3_codex_file_change_paths_extracts_changed_files() {
        // A settled codex `file_change` carries changes[].path — the SA3 write
        // signal reused from codex_collect_summary.
        let ev = json!({
            "type": "item.completed",
            "item": {
                "type": "file_change",
                "status": "completed",
                "changes": [
                    { "path": "src/a.rs", "kind": "modified" },
                    { "path": "src/b.rs", "kind": "added" },
                ]
            }
        });
        assert_eq!(
            codex_file_change_paths(&ev),
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()]
        );
        // A non-file_change item, or an in-progress one, yields nothing.
        let other = json!({
            "type": "item.completed",
            "item": { "type": "agent_message", "text": "done" }
        });
        assert!(codex_file_change_paths(&other).is_empty());
        let inprog = json!({
            "type": "item.started",
            "item": { "type": "file_change", "changes": [{ "path": "x" }] }
        });
        assert!(codex_file_change_paths(&inprog).is_empty());

        // And the extracted path auto-claims for a roommate (shared add_claim path).
        let root = delivery_tmp_root();
        let workdir = "/tmp/sa3-codex";
        let room = sa3_record_session(&root, "code-cdx00001", workdir);
        for p in codex_file_change_paths(&ev) {
            auto_claim_write(&root, "code-cdx00001", &p, Some(workdir));
        }
        let peers = codesession::peer_claims(&root, &room, "code-peerc000").unwrap();
        assert_eq!(peers.len(), 2, "both changed files claimed; saw {peers:?}");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn codex_apply_patch_paths_extracts_verified_hook_patch_headers() {
        let command = "*** Begin Patch\n\
*** Add File: note2.txt\n\
+kiwi\n\
*** Update File: src/lib.rs\n\
@@\n\
-old\n\
+new\n\
*** Delete File: old.txt\n\
*** Update File: before.txt\n\
*** Move to: after.txt\n\
*** End Patch\n";
        assert_eq!(
            codex_apply_patch_paths(command),
            vec![
                "note2.txt".to_string(),
                "src/lib.rs".to_string(),
                "old.txt".to_string(),
                "before.txt".to_string(),
                "after.txt".to_string(),
            ]
        );
    }

    #[test]
    fn sa3_opencode_file_write_path_extracts_edit_and_write() {
        // A settled opencode edit/write tool_use carries state.input.filePath
        // (REAL opencode 1.17.9 envelope, trimmed from a live `run --format json`
        // stream — Bug2). A revert to the old inferred `path` key MUST fail here.
        let edit = json!({
            "type": "tool_use",
            "sessionID": "ses_10e1f5066ffeqleVO7g1PDWk74",
            "part": {
                "type": "tool",
                "tool": "edit",
                "callID": "call_35aad0e169ba4e55bbdce02d",
                "state": { "status": "completed", "input": { "filePath": "/p/a.rs" } }
            }
        });
        assert_eq!(opencode_file_write_path(&edit), Some("/p/a.rs"));
        let write = json!({
            "type": "tool_use",
            "sessionID": "ses_10e1f5066ffeqleVO7g1PDWk74",
            "part": {
                "type": "tool",
                "tool": "write",
                "callID": "call_55fb8c75401547ee94fd5223",
                "state": { "status": "completed", "input": { "filePath": "/p/b.rs", "content": "brand new" } }
            }
        });
        assert_eq!(opencode_file_write_path(&write), Some("/p/b.rs"));
        // Legacy `path` fallback still resolves (older opencode binary).
        let legacy = json!({
            "type": "tool_use",
            "part": { "tool": "edit", "state": { "input": { "path": "/p/legacy.rs" } } }
        });
        assert_eq!(opencode_file_write_path(&legacy), Some("/p/legacy.rs"));
        // A non-writer tool (read) carries filePath too but is NOT a write: None.
        let read = json!({
            "type": "tool_use",
            "part": { "tool": "read", "state": { "input": { "filePath": "/p/c.rs" } } }
        });
        assert_eq!(opencode_file_write_path(&read), None);
        let text = json!({ "type": "text", "part": { "text": "hi" } });
        assert_eq!(opencode_file_write_path(&text), None);

        // And the extracted path auto-claims for a roommate (shared add_claim path).
        let root = delivery_tmp_root();
        let workdir = "/tmp/sa3-oc";
        let room = sa3_record_session(&root, "code-oc000001", workdir);
        if let Some(p) = opencode_file_write_path(&edit) {
            auto_claim_write(&root, "code-oc000001", p, Some(workdir));
        }
        let peers = codesession::peer_claims(&root, &room, "code-peero000").unwrap();
        assert_eq!(peers.len(), 1, "the edited file is claimed; saw {peers:?}");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn sa3_auto_claimed_path_surfaces_in_a_siblings_turn_injection() {
        // SA2's per-turn injection reads peer_claims; confirm an AUTO-claim (no
        // `elanus code claim`) surfaces for a sibling in the same room.
        let root = delivery_tmp_root();
        let workdir = "/tmp/sa3-inject";
        // Two sessions in the same checkout (same default workdir-room).
        let _room_a = sa3_record_session(&root, "code-inja0001", workdir);
        let _room_b = sa3_record_session(&root, "code-injb0002", workdir);

        // A writes a file via a tool (never claims).
        auto_claim_write(&root, "code-inja0001", "App.tsx", Some(workdir));

        // B's turn injection mentions A's auto-claimed path.
        let inj = turn_injection(&root, claude_agent_noun(), "code-injb0002")
            .expect("B should see a sibling/peer-claim line");
        let want = std::fs::canonicalize(Path::new(workdir).join("App.tsx"))
            .unwrap_or_else(|_| Path::new(workdir).join("App.tsx"))
            .to_string_lossy()
            .to_string();
        assert!(
            inj.contains(&want) || inj.contains("App.tsx"),
            "B's injection must surface A's auto-claimed path; got:\n{inj}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── M4: coding-agent memory-block projection ─────────────────────────────

    /// Record a Claude-Code session so turn_injection/note_owner can resolve it.
    fn m4_record(root: &Root, session: &str) {
        codesession::upsert_record(
            root,
            &codesession::SessionRecord {
                elanus_session: session.into(),
                native_session: format!("native-{session}"),
                tool: "claude".into(),
                agent_noun: claude_agent_noun().into(),
                workdir: String::new(),
                room: None,
            },
        )
        .unwrap();
    }

    /// Upsert a memory block for a coding session via the live store.
    fn m4_block(
        root: &Root,
        session: &str,
        name: &str,
        content: &str,
        priority: i32,
        session_scope: bool,
    ) {
        let conn = crate::db::open(root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let mut b =
            crate::context_blocks::ContextBlock::new(name, content, claude_agent_noun());
        b.scope = if session_scope {
            crate::context_blocks::Scope::Session
        } else {
            crate::context_blocks::Scope::Agent
        };
        b.priority = priority;
        crate::context_store::upsert_block(&conn, "default", &b, session, None).unwrap();
    }

    #[test]
    fn turn_injection_renders_session_memory_blocks() {
        let root = delivery_tmp_root();
        m4_record(&root, "code-blk00001");
        m4_block(
            &root,
            "code-blk00001",
            "identity",
            "I am Lily, the worker.",
            0,
            false,
        );
        m4_block(
            &root,
            "code-blk00001",
            "focus",
            "Ship memory-blocks M4.",
            0,
            true,
        );

        let inj = turn_injection(&root, claude_agent_noun(), "code-blk00001")
            .expect("blocks should produce an injection");
        assert!(inj.contains("[elanus block: identity]"), "got:\n{inj}");
        assert!(inj.contains("I am Lily, the worker."), "got:\n{inj}");
        assert!(inj.contains("[elanus block: focus]"), "got:\n{inj}");
        assert!(inj.contains("Ship memory-blocks M4."), "got:\n{inj}");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn note_alias_shows_in_turn_injection_and_not_as_a_block_line() {
        // `elanus code note` writes the `note` block; turn_injection reads it as the
        // [elanus note] line (old behavior), NOT as a generic [elanus block: note].
        let root = delivery_tmp_root();
        m4_record(&root, "code-note0009");
        codesession::set_note(&root, "code-note0009", "remember the rename").unwrap();
        // Round-trips through the block store.
        assert_eq!(
            codesession::get_note(&root, "code-note0009")
                .unwrap()
                .as_deref(),
            Some("remember the rename")
        );
        let inj = turn_injection(&root, claude_agent_noun(), "code-note0009")
            .expect("a note should produce an injection");
        assert!(
            inj.contains("[elanus note] remember the rename"),
            "got:\n{inj}"
        );
        assert!(
            !inj.contains("[elanus block: note]"),
            "the note must not also render as a generic block line; got:\n{inj}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn mid_cycle_injection_emits_once_then_dedups() {
        let root = delivery_tmp_root();
        m4_record(&root, "code-mid00001");
        // A high-priority (mid-cycle) block.
        m4_block(
            &root,
            "code-mid00001",
            "alert",
            "STOP: schema migrated",
            crate::context_store::MID_CYCLE_PRIORITY,
            true,
        );

        // First tool boundary: emitted.
        let first = mid_cycle_injection(&root, claude_agent_noun(), "code-mid00001")
            .expect("a fresh high-priority block emits mid-cycle");
        assert!(first.contains("[elanus block: alert]"), "got:\n{first}");
        assert!(first.contains("STOP: schema migrated"), "got:\n{first}");

        // Second tool boundary (unchanged): nothing (dedup).
        assert!(
            mid_cycle_injection(&root, claude_agent_noun(), "code-mid00001").is_none(),
            "an unchanged mid-cycle block must not re-emit on the next tool call"
        );

        // A NORMAL block does NOT ride mid-cycle.
        m4_block(&root, "code-mid00001", "calm", "fyi only", 0, true);
        assert!(
            mid_cycle_injection(&root, claude_agent_noun(), "code-mid00001").is_none(),
            "a normal-priority block must not emit mid-cycle"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn capability_matrix_degrades_codex_and_opencode_to_next_turn() {
        // Claude Code can do both vectors.
        assert_eq!(
            achievable_vector(claude_agent_noun(), InjectionVector::MidCycle),
            InjectionVector::MidCycle
        );
        assert_eq!(
            achievable_vector(claude_agent_noun(), InjectionVector::NextTurn),
            InjectionVector::NextTurn
        );
        // Codex has no live hook bridge → a mid-cycle request DEGRADES to next-turn.
        assert_eq!(
            achievable_vector(codex_agent_noun(), InjectionVector::MidCycle),
            InjectionVector::NextTurn,
            "Codex must degrade mid-cycle to next-turn, not error or drop"
        );
        // opencode-headless degrades too (served path deferred).
        assert_eq!(
            achievable_vector(opencode_agent_noun(), InjectionVector::MidCycle),
            InjectionVector::NextTurn
        );
        // next-turn is always achievable everywhere.
        for noun in [
            claude_agent_noun(),
            codex_agent_noun(),
            opencode_agent_noun(),
        ] {
            assert_eq!(
                achievable_vector(noun, InjectionVector::NextTurn),
                InjectionVector::NextTurn
            );
        }
    }

    #[test]
    fn codex_high_priority_block_lands_next_turn_not_dropped() {
        // A Codex session with a high-priority block: it has NO mid-cycle vector, so
        // the block must still appear in the next-turn injection (degraded, not lost).
        let root = delivery_tmp_root();
        codesession::upsert_record(
            &root,
            &codesession::SessionRecord {
                elanus_session: "code-cdx00001".into(),
                native_session: "native-cdx".into(),
                tool: "codex".into(),
                agent_noun: codex_agent_noun().into(),
                workdir: String::new(),
                room: None,
            },
        )
        .unwrap();
        // Store the block under the codex agent noun (its identity).
        let conn = crate::db::open(&root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let mut b = crate::context_blocks::ContextBlock::new(
            "alert",
            "urgent for codex",
            codex_agent_noun(),
        );
        b.scope = crate::context_blocks::Scope::Session;
        b.priority = crate::context_store::MID_CYCLE_PRIORITY;
        crate::context_store::upsert_block(&conn, "default", &b, "code-cdx00001", None).unwrap();

        let inj = turn_injection(&root, codex_agent_noun(), "code-cdx00001")
            .expect("the high-pri block must land next-turn for codex");
        assert!(
            inj.contains("urgent for codex"),
            "degraded delivery must include the block; got:\n{inj}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── agent-comms (C2/C3/C4) ────────────────────────────────────────────────

    /// Insert an inbox delivery directly on a session's mailbox topic with a given
    /// priority, mirroring what a `deliver`/emit would record. Returns the event id.
    fn comms_mail(
        root: &Root,
        agent_noun: &str,
        session: &str,
        from: &str,
        message: &str,
        priority: i32,
    ) -> i64 {
        let conn = crate::db::open(root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let mailbox = format!(
            "in/agent/{}/{}",
            topic::encode_segment(agent_noun),
            topic::encode_segment(session),
        );
        let payload = json!({ "prompt": message }).to_string();
        conn.execute(
            "INSERT INTO events (type, payload, priority, sender, state)
             VALUES (?1, ?2, ?3, ?4, 'pending')",
            rusqlite::params![mailbox, payload, priority, from],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Insert a message on a room's shared channel topic (`in/group/<id>`).
    fn comms_channel_msg(root: &Root, room: &str, from: &str, message: &str) {
        let conn = crate::db::open(root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let topic = format!("in/group/{}", topic::encode_segment(room));
        let payload = json!({ "prompt": message }).to_string();
        conn.execute(
            "INSERT INTO events (type, payload, sender, state)
             VALUES (?1, ?2, ?3, 'pending')",
            rusqlite::params![topic, payload, from],
        )
        .unwrap();
    }

    #[test]
    fn c2_inbox_computed_block_present_with_mail_absent_when_empty() {
        let root = delivery_tmp_root();
        m4_record(&root, "code-inbx0001");
        // Empty inbox → no injection at all (the quiet turn is preserved).
        assert!(
            turn_injection(&root, claude_agent_noun(), "code-inbx0001").is_none(),
            "an empty inbox must produce no inbox block (quiet turn)"
        );
        // Deliver one normal-priority message.
        comms_mail(
            &root,
            claude_agent_noun(),
            "code-inbx0001",
            "scout",
            "please review PR 7",
            0,
        );
        let inj = turn_injection(&root, claude_agent_noun(), "code-inbx0001")
            .expect("unseen mail must produce an inbox block");
        // Rendered in the block shape (C2), with the count + a preview of the latest.
        assert!(inj.contains("[elanus block: inbox]"), "got:\n{inj}");
        assert!(inj.contains("1 new message(s)"), "got:\n{inj}");
        assert!(
            inj.contains("Latest from scout: please review PR 7"),
            "got:\n{inj}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn c3_high_priority_mail_emits_mid_cycle_once_then_dedups() {
        let root = delivery_tmp_root();
        m4_record(&root, "code-hipr0001");
        // A high-priority (>= default threshold 5) unseen delivery.
        comms_mail(
            &root,
            claude_agent_noun(),
            "code-hipr0001",
            "lily",
            "STOP: API changed",
            9,
        );
        // A normal one too — it must NOT ride the mid-cycle vector.
        comms_mail(
            &root,
            claude_agent_noun(),
            "code-hipr0001",
            "lily",
            "fyi later",
            0,
        );

        // First tool boundary: the high-pri mail is injected mid-cycle, once.
        let first = mid_cycle_mail_injection(&root, claude_agent_noun(), "code-hipr0001")
            .expect("high-priority unseen mail must reach mid-cycle");
        assert!(first.contains("Urgent mail"), "got:\n{first}");
        assert!(first.contains("STOP: API changed"), "got:\n{first}");
        assert!(
            !first.contains("fyi later"),
            "a normal message must not ride mid-cycle; got:\n{first}"
        );

        // Second tool boundary (unchanged): nothing — the same message is not
        // re-injected, and it was NOT marked seen, so it still shows next-turn.
        assert!(
            mid_cycle_mail_injection(&root, claude_agent_noun(), "code-hipr0001").is_none(),
            "the same high-pri message must not re-inject on the next tool call"
        );
        // Not marked seen: both deliveries still count in the next-turn inbox block.
        let inj = turn_injection(&root, claude_agent_noun(), "code-hipr0001")
            .expect("unseen mail still shows next-turn");
        assert!(
            inj.contains("2 new message(s)"),
            "mid-cycle delivery must not mark mail seen; got:\n{inj}"
        );

        // Codex degrades: it has no mid-cycle hook, so its high-pri mail rides the
        // next-turn inbox block instead (achievable_vector). Verify the matrix gate.
        assert_eq!(
            achievable_vector(codex_agent_noun(), InjectionVector::MidCycle),
            InjectionVector::NextTurn,
            "Codex degrades mid-cycle mail to next-turn"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn c3_below_threshold_mail_does_not_go_mid_cycle() {
        let root = delivery_tmp_root();
        m4_record(&root, "code-lopr0001");
        // priority 4 < default threshold 5 → next-turn only, never mid-cycle.
        comms_mail(
            &root,
            claude_agent_noun(),
            "code-lopr0001",
            "lily",
            "routine handoff",
            4,
        );
        assert!(
            mid_cycle_mail_injection(&root, claude_agent_noun(), "code-lopr0001").is_none(),
            "below-threshold mail must not ride the mid-cycle vector"
        );
        // But it still shows next-turn (the inbox block).
        let inj = turn_injection(&root, claude_agent_noun(), "code-lopr0001").unwrap();
        assert!(inj.contains("[elanus block: inbox]"), "got:\n{inj}");
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn c4_room_channel_block_is_room_scoped_and_bounded() {
        let root = delivery_tmp_root();
        crate::config_repo::init(&root).unwrap();
        // Opt the room "team-1" in, with a recent-N bound of 2.
        crate::config_repo::set_key(&root, COMMS_PACKAGE, "channels", r#"["team-1"]"#).unwrap();
        crate::config_repo::set_key(&root, COMMS_PACKAGE, "channel_recent_n", "2").unwrap();

        // An in-room session.
        codesession::upsert_record(
            &root,
            &codesession::SessionRecord {
                elanus_session: "code-room0001".into(),
                native_session: "native-room1".into(),
                tool: "claude".into(),
                agent_noun: claude_agent_noun().into(),
                workdir: String::new(),
                room: Some("team-1".into()),
            },
        )
        .unwrap();
        // Three channel messages; the recent-N=2 bound must keep only the last two.
        comms_channel_msg(&root, "team-1", "scout", "msg one");
        comms_channel_msg(&root, "team-1", "scout", "msg two");
        comms_channel_msg(&root, "team-1", "lily", "msg three");

        let inj = turn_injection(&root, claude_agent_noun(), "code-room0001")
            .expect("an in-room session with channel traffic gets a channel block");
        assert!(
            inj.contains("[elanus block: channel:team-1]"),
            "got:\n{inj}"
        );
        assert!(
            inj.contains("msg three"),
            "newest must be present; got:\n{inj}"
        );
        assert!(
            inj.contains("msg two"),
            "second-newest within the bound; got:\n{inj}"
        );
        assert!(
            !inj.contains("msg one"),
            "the recent-N bound must drop the oldest; got:\n{inj}"
        );

        // A session in a DIFFERENT room sees nothing of team-1's channel.
        codesession::upsert_record(
            &root,
            &codesession::SessionRecord {
                elanus_session: "code-room0002".into(),
                native_session: "native-room2".into(),
                tool: "claude".into(),
                agent_noun: claude_agent_noun().into(),
                workdir: String::new(),
                room: Some("other-room".into()),
            },
        )
        .unwrap();
        let inj2 = turn_injection(&root, claude_agent_noun(), "code-room0002");
        assert!(
            inj2.as_deref()
                .map_or(true, |s| !s.contains("channel:team-1")),
            "an out-of-room session must not see the channel; got:\n{inj2:?}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    #[test]
    fn c4_channel_requires_opt_in() {
        let root = delivery_tmp_root();
        crate::config_repo::init(&root).unwrap();
        // No opt-in: a session in a room with traffic still sees no channel block.
        codesession::upsert_record(
            &root,
            &codesession::SessionRecord {
                elanus_session: "code-noopt0001".into(),
                native_session: "native-noopt".into(),
                tool: "claude".into(),
                agent_noun: claude_agent_noun().into(),
                workdir: String::new(),
                room: Some("team-1".into()),
            },
        )
        .unwrap();
        comms_channel_msg(&root, "team-1", "scout", "hello room");
        let inj = turn_injection(&root, claude_agent_noun(), "code-noopt0001");
        assert!(
            inj.as_deref()
                .map_or(true, |s| !s.contains("channel:team-1")),
            "a room not opted in must surface no channel block; got:\n{inj:?}"
        );
        let _ = std::fs::remove_dir_all(&root.dir);
    }

    // ── Skill materialization into coding harnesses ───────────────────────────

    #[test]
    fn profile_flag_default_and_override() {
        // No flag → the "default" profile; args pass through untouched.
        let (p, rest) = take_profile_flag(&["-p".into(), "say hi".into()]);
        assert_eq!(p, "default");
        assert_eq!(rest, vec!["-p".to_string(), "say hi".to_string()]);

        // `--profile <name>` selects the profile and is stripped (value too).
        let (p, rest) = take_profile_flag(&[
            "--profile".into(),
            "dev".into(),
            "task".into(),
        ]);
        assert_eq!(p, "dev");
        assert_eq!(rest, vec!["task".to_string()]);

        // A bare trailing `--profile` (no value) keeps the default and is dropped.
        let (p, rest) = take_profile_flag(&["--profile".into()]);
        assert_eq!(p, "default");
        assert!(rest.is_empty());
    }

    #[test]
    fn link_skill_packages_symlinks_each_and_is_noop_when_empty() {
        let base = std::env::temp_dir().join(format!(
            "elanus-skilllink-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        // Two source package dirs, each with a SKILL.md.
        let pkg_a = base.join("src/alpha");
        let pkg_b = base.join("src/beta");
        std::fs::create_dir_all(&pkg_a).unwrap();
        std::fs::create_dir_all(&pkg_b).unwrap();
        std::fs::write(pkg_a.join("SKILL.md"), "alpha body").unwrap();
        std::fs::write(pkg_b.join("SKILL.md"), "beta body").unwrap();

        let skills = vec![
            ("alpha".to_string(), pkg_a.clone()),
            ("beta".to_string(), pkg_b.clone()),
        ];
        let target = base.join("skillroot/.claude/skills");
        link_skill_packages(&target, &skills).unwrap();

        // Each package is a symlink at <target>/<name>, resolving to the source dir,
        // and the SKILL.md is reachable THROUGH the link (so a harness scanning the
        // dir sees it). Symlink, not copy: the link target is the real package dir.
        for (name, src) in &skills {
            let link = target.join(name);
            assert!(
                std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
                "{name} should be a symlink"
            );
            assert_eq!(&std::fs::read_link(&link).unwrap(), src);
            assert!(link.join("SKILL.md").exists(), "{name}/SKILL.md via the link");
        }

        // Empty set: no dir is created, no error.
        let empty_target = base.join("never_made/skills");
        link_skill_packages(&empty_target, &[]).unwrap();
        assert!(!empty_target.exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn codex_skills_home_mirrors_auth_and_links_skills() {
        let base = std::env::temp_dir().join(format!(
            "elanus-codexhome-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        // A fake real ~/.codex with auth/config/version.
        let real_codex = base.join("real_codex");
        std::fs::create_dir_all(&real_codex).unwrap();
        std::fs::write(real_codex.join("auth.json"), "{\"token\":\"x\"}").unwrap();
        std::fs::write(real_codex.join("config.toml"), "model=\"gpt\"").unwrap();
        std::fs::write(real_codex.join("version.json"), "{\"v\":\"1\"}").unwrap();
        // A source skill package.
        let pkg = base.join("pkgs/git-release");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("SKILL.md"), "release body").unwrap();

        let root = Root {
            dir: base.join("root"),
        };
        let skills = vec![("git-release".to_string(), pkg.clone())];

        // Point dirs_next_home_codex() at the fake real home via CODEX_HOME.
        // (Test-local env mutation; restored after.)
        let prev = std::env::var_os("CODEX_HOME");
        std::env::set_var("CODEX_HOME", &real_codex);
        let home = build_codex_skills_home(&root, "code-test0001", &skills)
            .expect("codex home is built for every launch");
        match prev {
            Some(v) => std::env::set_var("CODEX_HOME", v),
            None => std::env::remove_var("CODEX_HOME"),
        }

        // Auth/version are mirrored by symlink (read in place — the secret stays in
        // the real home), and login content is reachable through the link.
        for entry in ["auth.json", "version.json"] {
            let link = home.join(entry);
            assert!(
                std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
                "{entry} should be a symlink into the real codex home"
            );
            assert_eq!(&std::fs::read_link(&link).unwrap(), &real_codex.join(entry));
        }
        // Config is copied, not symlinked, because elanus appends its managed hook.
        let config = home.join("config.toml");
        assert!(
            !std::fs::symlink_metadata(&config)
                .unwrap()
                .file_type()
                .is_symlink(),
            "config.toml must be a session-local copy"
        );
        let config = std::fs::read_to_string(config).unwrap();
        assert!(config.starts_with("model=\"gpt\""));
        assert!(config.contains("[[hooks.PostToolUse]]"));
        assert!(config.contains("matcher = \"*\""));
        assert!(config.contains("[[hooks.PostToolUse.hooks]]"));
        assert!(config.contains("type = \"command\""));
        assert!(config.contains(" code hook PostToolUse"));
        // The skill is linked under <home>/skills/<name>, scannable by codex.
        assert!(home.join("skills/git-release/SKILL.md").exists());

        // No skills still yields a home because hooks are always wanted.
        let hook_only_home = build_codex_skills_home(&root, "code-test0002", &[])
            .expect("codex hook-only home is built without skills");
        assert!(hook_only_home.join("config.toml").exists());
        assert!(!hook_only_home.join("skills").exists());

        let _ = std::fs::remove_dir_all(&base);
    }
}
