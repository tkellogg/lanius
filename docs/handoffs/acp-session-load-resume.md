---
status: planned
author: Claude Opus 4.8 (planner)
last-updated: 2026-07-07
---

# ACP resume — continuing a captured ACP session (A6)

**The promise.** A launched ACP agent (goose, gemini, codex-acp, …) can be
*resumed*: the daemon delivers a new message to an idle ACP coding session and
it continues where it left off, exactly like `claude`/`codex`/`opencode`
resume does today. A2 already records the native ACP session id; A6 makes the
resume path know what to do with it.

This is the deferred milestone from `acp-harness.md` (§A6). A1–A4 shipped;
A5 (real-agent validation) is in flight. A6 is the last piece of the ACP arc.

---

## Why resume doesn't "just work" for ACP (grounded)

Every resume in the system funnels through **`resume_capture`**
(`src/codeagent.rs:8111`). It reads the session record, mints a fresh scoped
token, builds the injected message (`build_resume_message`, `:8161`), and then
asks **`resume_command_for`** (`src/codeagent.rs:7963`) for a `(program, args)`
pair to spawn. That function is a **CLI table**:

- `claude` → `claude -p --resume <id> --output-format stream-json … <msg>`
- `codex`  → `codex exec resume <id> --json … <msg>`
- `opencode` → `opencode run --session <id> --format json … <msg>`

Each is a **one-shot CLI invocation** that streams JSON to stdout, captured by
`resume_stream_capture_for` (`:8005`). **ACP has no CLI resume.** ACP is a
long-lived **stdio JSON-RPC session**: you spawn the agent, `initialize`, then
`session/load` (or `session/resume`) the recorded session, then
`session/prompt` the new message. It does not fit a `(program, args)` row, and
its output is not a resume stream — it is the same `session/update`
notifications the launch driver already projects to obs.

So A6 is **a driver-mode addition, not a table row** — the ACP driver
(`drive_acp_session`, `src/acp.rs:190`) gains a "load" entry path, and
`resume_capture` routes ACP tools to it instead of the CLI table.

---

## Wonky bits / decisions to confirm (read first)

1. **`session/load` vs `session/resume` — the live spec has BOTH; pick one.**
   `docs/acp-wire-notes.md` (from the real `schema-v1.19.0` capture) lists
   `session/load` (line 29, "deferred to A6") **and** `session/resume` (line 30,
   "not in handoff draft; deferred"), and the captured `codex-acp` initialize
   advertised both `"loadSession": true` (line 92) and a `"resume": {}`
   capability (line 105). `session/load` **replays the full session history**
   back to the client as `session/update` notifications before you can prompt;
   `session/resume` may be the lighter "continue without replay" method. **The
   A6 plan below assumes `session/load`** (it's the one the acp-harness handoff
   named and the one whose semantics are documented in the wire notes), **but
   the implementer must confirm against the live spec which one goose/codex-acp
   actually honor** and whether `session/resume` avoids the replay burden. If
   `session/resume` is real and cleaner, use it and delete the replay-handling
   work (wonky bit 3).

2. **`loadSession` is OPTIONAL — gate on it and fail closed.** It is an
   advertised `AgentCapabilities` flag (`acp-wire-notes.md:57`,`:92`). An agent
   that does not advertise it cannot be resumed over ACP. **Decision: if the
   capability is absent, fail the resume with a clear message** ("this ACP agent
   does not support resume") — **do NOT silently `session/new` a fresh session.**
   A fresh session would drop the entire thread the human expects continued and
   would look like data loss. The driver already stores the full `initialize`
   result (`src/acp.rs:301`, `initialize_result`), so the capability is right
   there to check.

3. **`session/load` replays history — don't double-project it as obs.** Per the
   wire notes, on load the agent re-emits every past turn as `session/update`
   notifications, which the current driver **projects straight to obs**
   (`acp-wire-notes.md:39`; the projection is the same code path A2/A3 built).
   A naive load would re-stream the whole conversation onto the bus as if it
   were new, duplicating the session's history. **The load path must suppress
   (or tag `replay: true`) the `session/update` notifications that arrive
   between `session/load` and the first post-prompt update**, and only project
   the genuinely-new updates from the resumed turn. (Moot if wonky bit 1 chooses
   `session/resume` and it doesn't replay.)

4. **The ACP adapter writes NO capture summary today — resume needs one.** Every
   other adapter writes `adapter-summary.json` via
   `write_capture_summary_file(ctx.summary_file(), …)`, which the parent reads
   (`read_capture_summary_file`, `src/codeagent.rs:7444`) to surface the
   worker's final text + file changes. **`src/acp.rs` never calls
   `write_capture_summary_file`** (grep confirms) — the ACP session is observed
   only via obs. That is tolerable-ish for launch (the obs stream carries the
   content) but a *resume* returns a `ResumeOutcome` whose summary the daemon
   relays back to the requester, so ACP resume would relay an empty result.
   **Decision: have `drive_acp_session` write a capture summary** — the final
   agent message text (the last `session/update` of kind
   `agent_message_chunk`, already buffered for obs) as the summary text, plus
   any files the agent reported. Do this for **both** launch and resume so the
   two paths are symmetric; it also closes a quiet launch-side gap. Small,
   localized, and it makes the resume outcome well-defined.

5. **Env plumbing mirrors A4.** A4's launcher stamps `LANIUS_ACP_ARGV` (the
   agent command; `src/acp.rs:13`, set in the launcher ~`src/codeagent.rs:3657`).
   Add **`LANIUS_ACP_LOAD_SESSION=<native id>`** on the resume spawn;
   `drive_acp_session` branches on its presence: set → `session/load` that id;
   absent → `session/new` as today. The injected resume message
   (`build_resume_message`, which prepends the per-turn `[lanius]` inbox/memory
   block) becomes the `session/prompt` text — thread it through unchanged so
   ACP resume gets the same per-turn context injection the CLI adapters get
   (`src/codeagent.rs:8151-8161`).

---

## Milestones

Ship together (a half-done resume path is worse than none), but each is
independently reviewable.

### A6.1 — driver "load" mode + the capability gate
- `drive_acp_session` (`src/acp.rs:190`) gains a load entry: when
  `LANIUS_ACP_LOAD_SESSION` is set, after `initialize` check
  `initialize_result.agentCapabilities.loadSession` (or the `session/resume`
  capability per wonky bit 1). If absent → bail with a clear, non-panicking
  error surfaced to the parent. If present → send `session/load`
  (`{ sessionId, cwd, mcpServers }` — reuse the A4 `build_mcp_servers` output,
  `acp-wire-notes.md:132`), absorb/tag the replayed `session/update` burst
  (wonky bit 3), then `session/prompt` the injected message and drive to
  `stopReason` exactly as the new-session path does.
- Reuse the existing frame source/sink seam so this is unit-testable.
- **Acceptance:** extend A1's scripted fake ACP agent to (a) advertise
  `loadSession`, (b) answer `session/load` and replay two historical
  `session/update`s, (c) complete a `session/prompt`. A unit test drives a load,
  asserts the replayed updates are NOT re-projected as new obs, the new turn IS,
  and a summary is produced. A second test: a fake that does NOT advertise
  `loadSession` → the driver fails closed with the documented message and does
  not fall back to `session/new`. `cargo test` green.

### A6.2 — route `resume_capture` to the adapter (not the CLI table)
- In `resume_capture` (`src/codeagent.rs:8111`), branch on whether
  `rec.tool` is an ACP-harness agent (its `harness_id_for_tool` resolves to the
  `harness-acp` package). For ACP: spawn the **`harness-acp` adapter** the way a
  launch does (`launch_external_harness` path), stamping `LANIUS_ACP_ARGV`
  (from the agent's manifest block, as A4 does) **and**
  `LANIUS_ACP_LOAD_SESSION=<rec.native_session>`, and harvest the summary the
  adapter now writes (A6.1 / wonky bit 4) via `read_capture_summary_file`.
  `resume_command_for` (`:7963`) and `resume_stream_capture_for` (`:8005`)
  either gain an ACP branch or are skipped for ACP by an earlier fork —
  keep the CLI-table code for claude/codex/opencode byte-identical.
- The resume marker obs (`session/resume`, `:8170`) and the fresh-token mint
  (`:8137`) are transport-agnostic — leave them as-is; they already work.
- **Acceptance:** with a stubbed/fake ACP agent wired as a manifest block, a
  `resume_capture` on a recorded ACP session spawns the adapter in load mode,
  the injected message reaches `session/prompt`, and the `ResumeOutcome` carries
  the agent's final text (not empty). The claude/codex/opencode resume tests are
  unchanged and still pass.

### A6.3 — real-agent validation + docs + residuals
- If A5 found a real ACP agent that advertises `loadSession` (goose or
  codex-acp), drive a **launch then resume** end-to-end: start a session, let it
  answer, then deliver a follow-up that references the first turn; confirm the
  agent continued the same thread (not a cold start).
- Update `docs/coding-harness-onboarding.md` and `acp-harness.md` (flip §A6 from
  deferred to shipped; move "Resume deferred to A6" out of the residuals list).
- **Acceptance:** a real ACP agent continues a captured session across a resume
  with a follow-up that depends on prior context; docs updated; residuals named.

---

## Deliberate KEEPS / non-goals

- The CLI resume table for claude/codex/opencode stays exactly as-is — ACP is an
  added branch, not a rewrite.
- No interactive/TUI resume (ACP v1 is headless-only here, per acp-harness.md).
- No mid-turn injection beyond the resume-prompt prepend (ACP can't, same as
  codex — acp-harness.md residuals).

## Honest residuals

- **Agents without `loadSession` (or `session/resume`) cannot be resumed** —
  a hard capability limit, surfaced as a clear error, not a silent cold start.
- **Replay handling is best-effort** — if an agent's `session/load` replay does
  not cleanly delimit "history" from "the new turn," the obs suppression may
  need a heuristic (e.g. stop suppressing at the first post-prompt update).
  Nail this against the real agent in A6.3.
- **`session/resume` vs `session/load`** may want revisiting once more agents are
  tested — different agents may implement only one.

## Read these first

- `src/codeagent.rs:8111` `resume_capture` — the resume entry the daemon calls.
- `src/codeagent.rs:7963` `resume_command_for` + `:8005`
  `resume_stream_capture_for` — the CLI table ACP must branch around.
- `src/acp.rs:190` `drive_acp_session` — the driver; `:301` stores the full
  `initialize` result (the capability lives there); `:324-343`
  session/new→record→prompt is the shape the load path mirrors.
- `src/acp.rs:635` `build_mcp_servers` — reuse for `session/load`'s `mcpServers`.
- `docs/acp-wire-notes.md` — lines 29-30 (`session/load`/`session/resume`),
  57/92 (`loadSession` capability), 39 (`session/update`→obs), 132
  (`LoadSessionRequest` fields).
- `docs/handoffs/acp-harness.md` §A6 + the A4-decision Log entry.

## Log

- 2026-07-07 (Opus, planner): wrote the handoff. Grounded every claim in the
  worktree code. Key calls: ACP resume is a driver "load" mode, not a
  `resume_command_for` row; gate on the optional `loadSession` capability and
  **fail closed** rather than cold-start; the driver must **write a capture
  summary** (it doesn't today) so the resume outcome isn't empty; suppress the
  `session/load` history replay from obs. Flagged the `session/load` vs
  `session/resume` fork for the implementer to settle against the live spec /
  the real agent. Depends on A5 having found a `loadSession`-capable agent for
  A6.3; A6.1/A6.2 are testable against the scripted fake without one.
