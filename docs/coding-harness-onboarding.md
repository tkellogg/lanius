---
status: reference
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-29
---

# Onboarding a new coding harness

You want to drive a new coding tool (gemini-cli, aider, cursor-cli, …) through elanus
the way `claude`/`codex`/`opencode` are: `elanus code <tool> "<task>"`, captured to the
bus, resumable, briefed, dispatchable, skill-equipped, sibling-aware.

## Start here: build an adapter, don't fork elanus

The intended way to add a harness is to **write a small adapter and ship it as a
package** — no elanus PR. Your adapter is the tool-specific 20% (launch the tool, read
its event stream); an `elanus-harness` SDK is the shared 80% (session identity, the
bus, edit-claims, comms). elanus hands your adapter a session (id, bus token, workdir,
mode, prompt, briefing, skills dir); you launch your tool, and for each event call
`ctx.emit(...)` / `ctx.claim(path)` / `ctx.record(native_id)`. Declare `[[harness]]` in
your package's `elanus.toml` and `elanus code <yourtool>` discovers it.

> **Status:** the adapter SDK + package dispatch is the TARGET shape, speced in
> [handoffs/pluggable-coding-harness.md](handoffs/pluggable-coding-harness.md) (why:
> [journeys/13-adding-a-harness-without-forking.md](journeys/13-adding-a-harness-without-forking.md)).
> Until it lands, harnesses are added in-tree against the `Harness` trait (below).
> Either way the REQUIREMENTS are the same — what your adapter must expose and emit —
> so the capability ladder and checklist below are what you implement regardless of
> whether you ship a package or (for now) a trait impl. Read the requirements as "what
> my adapter does," not "what I edit in elanus."

## The in-tree seam (how the built-ins work today): one trait, one registry line

Adding a built-in harness is **one `impl Harness` + one entry in `HARNESSES`**
(`src/codeagent.rs`). The launch envelope never matches on a concrete tool; it drives
everything off the trait. The structs are zero-sized (`&'static dyn`).

```
static HARNESSES: &[&dyn Harness] = &[&ClaudeCode, &Codex, &OpenCode /*, &YourTool */];
```

Trait surface (`src/codeagent.rs` ~415): `id`, `aliases`, `agent_noun` (the
`obs/agent/<noun>/…` bus identity), `binary`, `capture(mode) -> Capture`,
`mode_for(headless) -> Mode`, `settings` (hook config or None), `map_event`,
`resume_command`, `interactive_resume_args`, and the capture runners
(`run_stream_capture`, `run_tui_rollout_import`, `run_tui_server_events`,
`resume_stream_capture`).

## The two axes

**Mode** (what the human asked for): `Tui` (bare `elanus code <tool>`, interactive,
inherited stdio) vs `Headless` (`--headless`, captured one-shot worker).

**Capture** (how elanus observes that mode) — pick per `(tool, mode)` cell:

| Capture | Live? | What it needs from the tool | Used by |
|---|---|---|---|
| `HookBridge` | ✅ live | a **hook system** that fires on tool calls + a way to point it at a generated config | claude (both cells); **codex TUI** (hook bridge) |
| `StreamJson` | ✅ live | a **non-interactive JSON event stream** (`--json`/`--format json` on a one-shot run) | codex/opencode headless |
| `ServerEvents` | ✅ live | a **client/server model**: a `serve` that emits an event stream (SSE/WS) you can subscribe to + an `attach` so the human still drives a TUI | opencode TUI |
| `RolloutImport` | ❌ post-hoc | only an on-disk **session transcript** written as it runs; imported AFTER exit | codex TUI fallback |
| `Lifecycle` | ❌ brackets only | nothing — just start/stop brackets when there's no observable channel at all | (floor; avoid) |

**The ranking is the requirement ladder.** Prefer a live cell. A tool with hooks or a
JSON stream or a served event stream gets real-time capture (and real-time
sibling-awareness / auto-claims). A tool that exposes only a transcript file gets
post-hoc `RolloutImport` — usable, but its events are not live (siblings can't see it
until it exits). A tool that exposes nothing falls to `Lifecycle` brackets.

## Capability checklist — what a harness must expose, by concern

For each concern: what's required, and how the three existing harnesses solve it (so
you can pattern-match a new tool).

### 1. Live observation (capture) — REQUIRED for real-time features
The single most important question: **how can a supervising process watch this tool's
tool-calls/edits as they happen?** Options, best first:
- **Hooks** — claude (`settings.json` PreToolUse/PostToolUse → `elanus code hook`);
  **codex** (`config.toml [[hooks.PostToolUse]]` on `apply_patch`, run with
  `--dangerously-bypass-hook-trust`; the path is inside `tool_input.command`).
- **A JSON event stream** on a non-interactive run — `codex exec --json`,
  `opencode run --format json`. Parse the JSONL, map to obs leaves in `map_event`.
- **A served event stream** for the interactive case — `opencode serve` + SSE `/event`
  + `opencode attach`. elanus runs its own server, subscribes, and attaches the human.
- **Fallback:** an on-disk transcript imported post-hoc (codex rollout).
If a tool has none of these, you only get lifecycle brackets — fine for "it ran", not
for "what is it doing".

### 2. Identity & auth isolation — REQUIRED
The tool brings its OWN provider auth; elanus must NOT leak its own provider creds into
it, and SHOULD isolate it from the user's global tool config.
- **Scrub** `PROVIDER_CRED_VARS` (ANTHROPIC_*/OPENAI_* …) from the child before exec, so
  the tool uses its own login, not elanus's DeepSeek/etc. env (`scrub_provider_creds`).
- **Isolate from the user's global config:** claude `--setting-sources ''`; codex a
  per-session `CODEX_HOME` (auth symlinked in so login survives); opencode `--pure` +
  `OPENCODE_CONFIG_DIR`. Find the tool's "don't load my global config / use this home
  instead" lever.
- Set elanus's own session identity in the child env (`ELANUS_CODE_SESSION`,
  `ELANUS_AGENT`, `ELANUS_ROOT`) so `elanus code` sub-invocations (deliver/claim/hook)
  resolve. For a hook bridge the hook reads the elanus session from ENV, never from the
  tool's native session id.

### 3. Skills / context materialization — for skill-equipped sessions
Deliver a profile's visible skills into the tool's native skills surface (see
[handoffs/coding-skill-materialization.md](handoffs/coding-skill-materialization.md)).
Each tool has a different scan location, all fed the same `SKILL.md` packages by symlink
into a per-session scratch:
- claude: `--plugin-dir <scratch>/plugin` (a generated `.claude-plugin/plugin.json` +
  `skills/`) — note `--add-dir` does NOT register skills and `--setting-sources ''`
  disables `.claude/skills` discovery.
- codex: `$CODEX_HOME/skills/<name>/` (the per-session home).
- opencode: `$OPENCODE_CONFIG_DIR/skills/<name>/`.
For a new tool: find where it scans for agent skills and whether you can point it at an
arbitrary per-session dir. (Most have converged on the agentskills.io `SKILL.md` format.)

### 4. Briefing injection (the launch envelope) — OPTIONAL but expected
Inject elanus's per-session briefing out-of-band: claude `--append-system-prompt`;
codex prepends it to the prompt / pipes on stdin (no system-prompt flag); opencode
folds it into the message. Find the tool's "extra system/context" channel; degrade to
prepending the user prompt if none.

### 5. Model / provider selection — OPTIONAL
How to point the tool at a chosen model/effort (claude `--model`; codex `-c model=…`,
`-c model_reasoning_effort=…`; opencode `--model provider/model`). Wire through the
`--provider` materialization if the tool can take a provider override
([handoffs/model-providers.md](handoffs/model-providers.md)).

### 6. Resume — OPTIONAL but valued
A way to re-enter a prior native session headlessly (`resume_command`) and, ideally, an
interactive passthrough (`interactive_resume_args`). Requires the tool to expose a
stable native session/thread id (codex thread id in the rollout; opencode `sessionID`;
claude session id) that elanus records and can resume against.

## Decision tree for the capture strategy

```
Does the tool have a hook system that fires on tool/file events?
  └─ yes → HookBridge for BOTH cells (best; live even in the TUI). [claude, codex-TUI]
Else, does it have a non-interactive JSON event stream (--json run)?
  └─ yes → StreamJson for the Headless cell. [codex, opencode headless]
For the interactive TUI cell, in order:
  ├─ client/server with a subscribable event stream + attach? → ServerEvents (live). [opencode]
  ├─ writes a session transcript file? → RolloutImport (post-hoc, not live). [codex rollout]
  └─ none of the above? → Lifecycle brackets only (last resort).
```

## Onboarding checklist
- [ ] `impl Harness` + add to `HARNESSES`; pick `agent_noun`, `aliases`, `binary`.
- [ ] `mode_for` + `capture(mode)` per the decision tree above.
- [ ] Headless capture: implement the JSONL/event parse in `map_event` + the runner.
- [ ] TUI capture: hook bridge / served-events / rollout import, per the tree.
- [ ] Auth: scrub provider creds; isolate from global config; set ELANUS_* session env.
- [ ] (If skills) materialize the profile's `SKILL.md` packages into the tool's skills dir.
- [ ] (If briefing) wire the launch-envelope briefing into the tool's context channel.
- [ ] (If sibling-aware) call `auto_claim_write` from the tool's write-tool detection so
      its edits become advisory claims (hook handler / stream / SSE — wherever you see
      edits live).
- [ ] Resume: record the native session id; implement `resume_command`.
- [ ] Verify: a real `--headless` run captures to `obs/agent/<noun>/<session>/…`; a TUI
      run is observable per its cell; a sibling sees its edits (if live).

## The "remaining dozen"
Likely candidates and the lever each is bet to expose (verify before building — this
session's lesson is that capability assumptions rot fast):
- **aider** — non-interactive run + a transcript; probably `StreamJson`-ish or
  Lifecycle; check for a JSON/event mode.
- **gemini-cli**, **cursor-cli**, **cline/continue (mostly IDE)**, **amp**, **goose**,
  **crush**, **qwen-coder**, … — for each, ask the six questions above, but START with
  #1 (live observation): does it have hooks, a `--json` stream, or a serve/attach? That
  one answer places it on the capture ladder and decides how much of the journey-12
  sibling-awareness it can support.

**Hard-won caveat:** do not assume a tool's capabilities from memory or an old version.
This session shipped a wrong "codex has no hooks / its TUI is unobservable" claim that a
30-second smoke test refuted (codex has a full hook system). Smoke-test the lever before
you design the cell.
