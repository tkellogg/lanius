---
status: reference
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-29
---

# Onboarding a new coding harness

You want to drive a new coding tool (gemini-cli, aider, cursor-cli, …) through elanus
the way `claude`/`codex`/`opencode` are: `elanus code <tool> "<task>"`, captured to the
bus, resumable, briefed, dispatchable, skill-equipped, sibling-aware.

**There is one way to do this: ship a harness as a package** — the same distribution
mechanism as every other elanus capability (a skill, a provider, a stage, a daemon
actor). You write a small adapter binary, drop it in your elanus root's `packages/`
with an `elanus.toml`, grant it, and `elanus code <tool>` runs it. No fork of elanus,
no PR.

> **Status:** the `[[harness]]` manifest + dispatch + the `elanus-harness` adapter SDK
> are speced in [handoffs/pluggable-coding-harness.md](handoffs/pluggable-coding-harness.md)
> (why: [journeys/13-adding-a-harness-without-forking.md](journeys/13-adding-a-harness-without-forking.md))
> and not yet implemented. This is the canonical recipe for that one way. (The three
> built-ins are currently in-tree trait impls; they are migrating to packages — you do
> not write a trait.)

## What you build

A **harness package** is a directory:

```
harness-gemini/                 # the package (dir name is convention: harness-<tool>)
├── elanus.toml                 # the manifest: declares [[harness]] + bus grants
└── bin/
    └── adapter                 # your adapter binary (any name; the manifest points at it)
```

Your **adapter** is a small program (write it in Rust against the `elanus-harness`
crate — the supported path; any executable that speaks the obs contract works). elanus
mints the session and hands the adapter its identity (env); the adapter launches the
tool, watches it, and reports. The whole thing is roughly:

```rust
fn main() -> elanus_harness::Result<()> {
    let ctx = elanus_harness::Ctx::from_env()?;   // session id, bus token, root, workdir,
                                                  // mode, prompt, briefing, skills dir
    let mut child = launch_gemini(&ctx)?;         // YOUR 20%: spawn the tool
    for ev in gemini_events(&mut child) {         // YOUR 20%: parse its stream/hooks/SSE
        ctx.emit(ev.leaf, ev.body);               // → obs/agent/<noun>/<session>/<leaf>
        if let Some(p) = ev.edited_path { ctx.claim(&p); }       // advisory edit-claim
        if let Some(id) = ev.native_session { ctx.record(id); }  // durable resume record
    }
    ctx.finish(child.wait()?)
}
```

The SDK is the shared 80% (identity, the bus, claims, comms, last-active); your adapter
is the tool-specific 20% (launch + translate). See the SDK surface in the handoff.

## The manifest: `elanus.toml`

```toml
# Declares this package as a coding harness. `elanus code gemini …` resolves here.
[[harness]]
name       = "gemini"            # the verb: `elanus code gemini`
aliases    = ["gem"]             # optional extra verbs
agent_noun = "gemini"            # the bus identity: obs/agent/gemini/<session>/…
run        = "bin/adapter"       # the adapter binary, RELATIVE to the package dir

# The bus authority the adapter needs (deny-by-default, like every package).
[request]
publish = ["obs/agent/gemini/#"] # to emit its session's observations
# add subscribe/comms grants here if the adapter uses the inbox/deliver rails
```

`run` is resolved relative to the package directory (same as a daemon package's
`[process].run`).

## Install it — from a fresh repo into your elanus root

Your elanus root defaults to **`~/.elanus/root`** (or `$ELANUS_ROOT`). Packages live in
**`<root>/packages/<name>/`**. Develop the adapter in its own git repo, then copy the
built binary + manifest into the root:

```sh
# In your adapter's own git repo:
cargo build --release                       # build the adapter binary

# Place it as a package in the elanus root (default ~/.elanus/root):
ROOT="${ELANUS_ROOT:-$HOME/.elanus/root}"
mkdir -p "$ROOT/packages/harness-gemini/bin"
cp target/release/adapter "$ROOT/packages/harness-gemini/bin/adapter"
cp elanus.toml            "$ROOT/packages/harness-gemini/elanus.toml"

# Grant it (packages are deny-by-default; this approves its requested bus authority):
elanus approve harness-gemini

# Use it:
elanus code gemini "refactor foo.rs"
elanus code gemini --headless "run the test suite and report"
```

That's it — same `packages/` + `elanus approve` flow as any other capability. To
distribute it to others, ship it as a **kit** (the existing distro: a `packages/` dir
the recipient `elanus kit install`s), exactly like a bundle of skills.

## What your adapter must do (the requirements)

These are the same regardless of which tool you wrap — they're "what the adapter
emits," not "what to edit in elanus."

### 1. Live observation (capture) — the load-bearing question
How can your adapter watch the tool's tool-calls/edits **as they happen**? Options,
best first — this decides how real-time the integration (and sibling-awareness) is:
- **Hooks** — claude (`settings.json` PreToolUse/PostToolUse → a command); **codex**
  (`config.toml [[hooks.PostToolUse]]` on `apply_patch`, run with
  `--dangerously-bypass-hook-trust`; the path is inside `tool_input.command`). Your
  adapter generates the hook config and turns each hook callback into `ctx.emit` /
  `ctx.claim`.
- **A non-interactive JSON event stream** — `codex exec --json`,
  `opencode run --format json`. Parse the JSONL; `ctx.emit` each event.
- **A served event stream** for the interactive case — `opencode serve` + SSE `/event`
  + `opencode attach`: run your own server, subscribe, attach the human.
- **Fallback:** an on-disk transcript imported after exit (codex rollout) — not live.
None of these → lifecycle brackets only ("it ran", not "what it's doing").

### 2. Identity & auth isolation — REQUIRED
- The tool brings its OWN provider auth; do NOT leak elanus's provider creds — the SDK
  gives you a `scrub_provider_creds` helper for the child.
- Isolate the tool from the user's global config: claude `--setting-sources ''`; codex a
  per-session `CODEX_HOME` (auth symlinked in); opencode `--pure` + `OPENCODE_CONFIG_DIR`.
  Find the tool's "use this home, not my global config" lever.
- **MERGE the user's MCP servers back in — do NOT throw them out with the hooks/plugins.**
  An MCP server is user-authority, not elanus-authority (safety = audit, not restriction):
  the user configured it for this tool, so an elanus launch must not silently drop it. Carry
  ONLY the MCP registrations; keep excluding hooks, plugins, and misc settings. Per harness
  (verified live — docs/handoffs/mcp-on-launch.md):
  - **claude** — the shadow is real: `--setting-sources ''` disables user/project settings
    and `.mcp.json`. Read the user-scope registry (`~/.claude.json` → `mcpServers`) and hand
    it back via `--mcp-config <generated-file>`, which COMPOSES with `--setting-sources ''`
    (confirmed on Claude Code 2.1.198). The `--settings` object stays hooks-only.
  - **codex** — NOT shadowed: `build_codex_skills_home` copies `config.toml` verbatim, so
    `[mcp_servers]` is carried and the server LOADS. (Caveat: `codex exec` headless auto-
    CANCELS an MCP tool call under its default approval/sandbox unless fully bypassed — a
    codex cage-policy matter, not a config shadow; the interactive TUI approves and works.)
  - **opencode** — NOT shadowed (on 1.17.9): `--pure` disables ONLY plugins, and
    `OPENCODE_CONFIG_DIR` does not shadow config-file MCP — the user's `~/.config/opencode`
    `mcp` block still loads and connects under the exact launch posture.
  Record the merged server names on `session/start` (record-not-gate). A read/parse failure
  degrades to no-user-MCP with ONE stderr line — never a launch failure.
- elanus sets the session identity in your adapter's env (`ELANUS_CODE_SESSION`,
  `ELANUS_AGENT`, `ELANUS_ROOT`, `ELANUS_BUS_TOKEN`); `Ctx::from_env` reads it. A hook
  the tool spawns inherits this env and resolves the elanus session from it (never from
  the tool's native session id).

### 3. Skills / context materialization — for skill-equipped sessions
elanus hands the adapter a skills dir; materialize it into the tool's native skills
scan location (see [handoffs/coding-skill-materialization.md](handoffs/coding-skill-materialization.md)):
claude `--plugin-dir` (a generated plugin); codex `$CODEX_HOME/skills/`; opencode
`$OPENCODE_CONFIG_DIR/skills/`. (Most tools take the agentskills.io `SKILL.md` format.)

### 4. Briefing injection — OPTIONAL but expected
Inject elanus's per-session briefing out-of-band: claude `--append-system-prompt`; codex
prepends/pipes on stdin; opencode folds it into the message. Degrade to prepending the
user prompt if the tool has no extra-context channel.

### 5. Model / provider selection — OPTIONAL
Point the tool at a chosen model/effort: claude `--model`; codex `-c model=…`,
`-c model_reasoning_effort=…`; opencode `--model provider/model`.

### 6. Resume — OPTIONAL but valued
`ctx.record(native_session_id)` the tool's stable native session/thread id so elanus can
resume it (codex thread id; opencode `sessionID`; claude session id).

## Capture decision tree
```
Does the tool have a hook system that fires on tool/file events?
  └─ yes → hooks for BOTH cells (best; live even in the TUI). [claude, codex]
Else, does it have a non-interactive JSON event stream (--json run)?
  └─ yes → stream it for the headless cell. [codex, opencode headless]
For the interactive TUI cell, in order:
  ├─ client/server with a subscribable event stream + attach? → served events (live). [opencode]
  ├─ writes a session transcript file? → import it post-hoc (not live). [codex rollout]
  └─ none of the above? → lifecycle brackets only (last resort).
```

## Checklist
- [ ] Pick the verb + `agent_noun`; write `elanus.toml` with `[[harness]]` + `[request]`.
- [ ] Adapter: `Ctx::from_env`, launch the tool, translate events → `ctx.emit`.
- [ ] Capture: per the decision tree (hooks / stream / served events / rollout).
- [ ] Auth: `scrub_provider_creds`; isolate from the tool's global config.
- [ ] MCP merge: carry the user's MCP registrations back into the session (see
      requirement #2) and record the merged server names on `session/start`.
- [ ] Edit-claims: `ctx.claim(path)` on every write the tool makes (this is what makes
      it sibling-aware).
- [ ] (If skills) materialize the handed-over skills dir into the tool's skills location.
- [ ] (If briefing) inject the briefing into the tool's context channel.
- [ ] Resume: `ctx.record(native_id)`.
- [ ] Install: copy binary + `elanus.toml` into `<root>/packages/harness-<tool>/`;
      `elanus approve`; verify `elanus code <tool> --headless …` captures to
      `obs/agent/<noun>/<session>/…` and a sibling sees its edits (if live).

## The "remaining dozen"
For each, ask the requirements above, but START with #1 (live observation): does it have
hooks, a `--json` stream, or a serve/attach? That one answer places it on the capture
ladder and decides how much sibling-awareness it can support.
- **aider** — non-interactive run + a transcript; check for a JSON/event mode.
- **gemini-cli**, **cursor-cli**, **cline/continue** (mostly IDE), **amp**, **goose**,
  **crush**, **qwen-coder**, …

**Hard-won caveat:** do not assume a tool's capabilities from memory or an old version.
This session shipped a wrong "codex has no hooks / its TUI is unobservable" claim that a
30-second smoke test refuted (codex has a full hook system). Smoke-test the lever before
you design the adapter.
