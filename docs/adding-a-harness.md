---
status: reference
author: coding-agent-tails HM5
last-updated: 2026-06-22
---

# Adding a coding harness

A coding harness (Claude Code, Codex, opencode) is an external coding agent
brought up from the command line. elanus runs **one envelope** — launch,
per-session grant-scoped identity, provider-cred scrub, the `obs/agent/<noun>/…`
grammar, mailbox, `spawn`/`deliver`, resume, the projection — that is
**harness-agnostic**. Adding a harness is implementing one adapter against the
`Harness` trait and registering it; the envelope is not touched.

This recipe is **proven**: opencode was added as a real third adapter against
exactly this seam (a third `impl Harness` + one line in `HARNESSES`), which is
the HM5 acceptance criterion from
[handoffs/harness-modes.md](handoffs/harness-modes.md) — "a third adapter
compiles against the trait without touching the envelope."

## The seam today (post HM1/HM2/HM3/OC3)

All of this lives in [src/codeagent.rs](../src/codeagent.rs).

- A `trait Harness` over **zero-sized structs** (`ClaudeCode`, `Codex`,
  `OpenCode`) held as `&'static dyn Harness` in a static `HARNESSES` registry.
  Resolution: `harness()` (by alias), `harness_by_noun()` (by obs noun),
  `harness_for_record()` (by recorded `tool`, Claude fallback),
  `parse_harness()` (CLI verb → harness, errors like the old `Tool::parse`).
- `enum Mode { Tui, Headless }` — the launch-mode axis (Tui = inherited stdio,
  human-pumped; Headless = captured, programmatic).
- `enum Capture { HookBridge, StreamJson, RolloutImport, ServerEvents,
  Lifecycle }` — the per-(harness, mode) capture mechanism, declared by the
  adapter, routed on by the launcher.

**The capture matrix as it stands** (the adapter's `capture(mode)` return):

| Capture | What it means | Who uses it |
|---|---|---|
| `HookBridge` | The child's **own hooks** call `elanus code hook`, which publishes. Launcher inherits stdio, parses nothing. Live, per-event. | Claude — both `Tui` and `Headless` |
| `StreamJson` | Launcher **pipes stdout**, parses a JSONL event stream in-process, publishes the obs record itself (as the session principal). Live. | Codex `Headless` (`codex exec --json`), opencode `Headless` (`opencode run --format json`) |
| `RolloutImport` | The TUI ran with inherited stdio; at **session stop** the launcher resolves the rollout JSONL the TUI wrote on disk and projects its turns. Post-hoc, coarser than live. Stamped `fidelity=rollout-import`. | Codex `Tui` |
| `ServerEvents` | Launcher starts a harness server, **subscribes its live SSE event stream**, runs the TUI against it, projects each event live. Stamped `fidelity=server-events-live`. | opencode `Tui` (client/server: `opencode serve` + `attach <url>`) |
| `Lifecycle` | The **bracketed floor**: only the launcher's `session/start` + `session/stop` are emitted, no per-turn detail. Honest fallback for a TUI we can launch but not capture. Currently unused (no harness selects it) but kept as the declared drop-down. | (none — the fallback) |

## To add a new harness

1. **Implement `Harness` for a new zero-sized struct.** Fill in `id()` (the
   canonical CLI verb, e.g. `"aider"`), `aliases()` (verb + short forms),
   `agent_noun()` (the `obs/agent/<noun>/…` segment), and `binary()` (the real
   executable). See `ClaudeCode` / `Codex` / `OpenCode` at codeagent.rs:414-711.

2. **Decide the `(mode → Capture)` matrix and implement `mode_for` +
   `capture(mode)`.** `mode_for(headless: bool)` maps the uniform `--headless`
   flag to a `Mode` (bare/prompt → `Tui`, `--headless`/`--worker` → `Headless`).
   `capture(mode)` returns the matrix cell per axis-1 mode. Reuse an existing
   variant where you can.

3. **Implement the per-mode launch.** For `StreamJson`/`RolloutImport`/
   `ServerEvents`/`Lifecycle`, override the matching `run_*` method
   (`run_stream_capture`, `run_tui_rollout_import`, `run_tui_server_events`,
   `run_tui_lifecycle` — defaults `unreachable!()`). Build argv + stdio + env
   inside it. Three things the envelope already guarantees and your launch
   must **not** undo:
   - **`scrub_provider_creds`** (codeagent.rs:164) removes elanus's provider
     env so the tool uses its OWN login — your `Command` runs through it.
   - **The briefing injection** — the envelope's per-turn context goes in via
     your harness's documented channel (Claude: generated `--settings` +
     `--add-dir` skill root, see `claude_settings`; Codex/opencode: prepended
     to the prompt, no `--append-system-prompt`). Pick the faithful channel
     for your tool.
   - **The cage** — workdir + identity + observation are elanus-owned; the
     tool keeps its own sandbox (no bypass, per the sandbox stance).

4. **Implement `settings()` and `map_event()`.** `settings()` returns the
   generated tool config that routes hooks through `elanus code hook`
   (`HookBridge` only — Claude returns `claude_settings(...)`; StreamJson
   harnesses return `None` and write nothing to the tool home).
   `map_event(event, payload)` maps one hook event to an `(obs_leaf, body)`
   pair. StreamJson harnesses map their stream directly and never reach the
   hook bridge; they file generically (`generic_event`, codeagent.rs:5487) as
   a safety net.

5. **Implement `resume_command(rec, message)` and `resume_stream_capture(...)`.**
   The daemon resume path (M2-B) drives a recorded session by spawning the
   native resume command (e.g. `codex exec resume <thread_id> --json …`,
   `opencode run --session <id> …`) and reading its stdout stream. Each
   harness owns its stream grammar; reuse the launch-stream reader if the
   resume stream matches (codex/opencode do).

6. **Register the struct** in `HARNESSES` (codeagent.rs:715) — one `&YourHarness`
   line. Aliases are parsed automatically via `harness()`; no separate table.

7. **If the capture SHAPE is novel** (not one of the existing 5 variants), add
   a **reader**. Three templates, all in codeagent.rs:
   - **StreamJson reader** — a stdout line-by-line JSONL parser:
     `capture_*_stream` (e.g. `capture_codex_stream`, `capture_opencode_stream`)
     driving `*_map_event` + `*_collect_summary` per line.
   - **RolloutImport reader** — a post-hoc on-disk importer that resolves the
     rollout file from the recorded native id and projects its turns (see
     `run_codex_tui_import` + `rollout_map_record`).
   - **ServerEvents reader** — a live SSE subscriber that projects each server
     event as it arrives (see `run_opencode_tui_server_events` +
     `opencode_sse_publish` + `opencode_sse_to_run_event`).

   **Stamp fidelity and source honestly.** Every projected event carries a
   `fidelity` / `source` marker (`rollout-import`, `server-events-live`, etc.)
   so a consumer never mistakes post-hoc or best-effort capture for live
   hook-bridged granularity. **Declare fidelity; don't fake it** — if the best
   you can do is `Lifecycle`, say so (see the guardrail in
   [harness-modes.md](handoffs/harness-modes.md) "Honesty / guardrails").

8. **Add tests mirroring the per-harness seam tests** (see
   `capture_strategy_and_agent_noun_per_tool` at codeagent.rs:5644 and the
   `opencode_map_event_projects_the_obs_grammar` /
   `opencode_collect_summary_harvests_final_text_and_changed_files` pair), and
   **update the front door**: `print_help` / `print_tools` /
   `DISPATCH_HINT` (codeagent.rs:101) and the `ELANUS_SKILL` cheatsheet so the
   new verb is discoverable.

That's the whole surface. The envelope — cage, identity, obs grammar, mailbox,
spawn/deliver, resume, projection — stays untouched.
