---
status: planned
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-07-03
---

# Handoff: the generic ACP harness — one adapter, every ACP agent

**The promise.** Today each coding tool needs its own hand-written adapter (claude,
codex, opencode). The Agent Client Protocol (ACP) is a standard JSON-RPC wire that
25+ agents already speak — Gemini CLI, Goose, Copilot CLI, Cline, OpenHands, and
(via `codex-acp`) codex itself. If elanus can speak ACP once, it onboards all of
them with a single adapter, and adding the next one becomes a six-line config file
instead of a new binary. This is the highest-leverage harness we can build: it
collapses journey 13's "remaining dozen" into one package.

The shape already exists in miniature. The codex app-server driver
(`drive_codex_app_server`, `src/codeagent.rs:4860`) is a worked ACP client in all
but name — a bidirectional JSON-RPC session over stdio that receives every event as
a notification and every permission request as a blocking call it relays to the
owner's mailbox. ACP is the same protocol with standardized method names. We are
generalizing a thing we already ship.

---

## Read these first
- `docs/coding-harness-onboarding.md` — the section **"The RPC-driver shape (ACP and
  its dialects)"** (~line 247). This handoff is that section, made concrete.
- `docs/handoffs/pluggable-coding-harness.md` — a harness IS a package with a
  `[[harness]]` manifest; the `Ctx` SDK (`src/harness.rs`) is what an adapter builds
  on. That machinery is done and merged; the ACP adapter is just another consumer.
- `docs/handoffs/codex-app-server.md` (referenced from the driver comments) — the
  approval-elicitation contract and the "hold-the-socket" reality we inherit.
- The live code we reuse:
  - `src/codeagent.rs:4860` `drive_codex_app_server` — the JSON-RPC state machine to
    clone the shape of (built to run against a real child **or** an in-process mock —
    reuse that test seam).
  - `src/codeagent.rs:5081` `codex_appserver_handle_approval` + `:5228`
    `codex_appserver_await_answer` — the elicitation relay (emit ask → poll
    correlation → fail-closed default). The security-critical part; we share it.
  - `src/codeagent.rs:5315` `codex_appserver_map_notification` / `:5388` `_map_item`
    — the notification → obs projection to mirror.
  - `src/harness.rs` — the `Ctx` SDK (`emit` / `claim` / `record` / `bump_active` /
    `scrub_provider_creds`) and the launch-env contract.
  - `src/bin/harness-codex.rs` — the 4-line adapter bin pattern the `acp` bin copies.
  - `src/initcmd.rs:107` `STOCK_HARNESS_PACKAGES` + `:648`
    `seed_stock_harness_packages` — how a stock harness package is seeded.
  - `src/manifest.rs:313` `HarnessDecl` — the `[[harness]]` manifest struct we extend.

---

## The spec, honestly (and the one thing you must re-verify)

**I could not fetch the live spec.** This session is sandboxed: `curl`, WebFetch, and
WebSearch were all blocked in non-interactive mode. Every ACP wire name below is from
training knowledge plus the onboarding doc — **treat them as a strong draft, not
gospel.** Milestone **A1 exists precisely to pin these against the real schema**
(`agentclientprotocol.com` → the JSON schema, and one real agent's actual handshake)
before any of it is load-bearing. This is a deliberate gate, not a guess dressed as
fact.

What ACP is (draft): **JSON-RPC 2.0 over stdio, newline-delimited** (one JSON object
per line — the same framing the codex app-server uses, so the reader is reusable).

**Client → agent (requests we send):**
- `initialize` `{protocolVersion, clientCapabilities}` → `{protocolVersion,
  agentCapabilities, authMethods}`. Capability handshake — where we learn whether the
  agent supports `loadSession`, HTTP/SSE MCP, images, etc.
- `authenticate` `{methodId}` — optional; only if the agent lists auth methods.
- `session/new` `{cwd, mcpServers[]}` → `{sessionId}`. **MCP servers are passed here.**
- `session/load` `{sessionId, cwd, mcpServers[]}` — resume a persisted session
  (replays history via `session/update`). Only if `agentCapabilities.loadSession`.
- `session/prompt` `{sessionId, prompt: ContentBlock[]}` → `{stopReason}`. One turn.
- `session/cancel` `{sessionId}` — notification.

**Agent → client (things we must answer or record):**
- `session/update` — **notification** `{sessionId, update}`. `update` is a union
  discriminated by `sessionUpdate`: `agent_message_chunk`, `agent_thought_chunk`,
  `user_message_chunk`, `tool_call` (`{toolCallId, title, kind, status, content,
  locations, rawInput}`), `tool_call_update`, `plan`, `available_commands_update`,
  `current_mode_update`.
- `session/request_permission` — **request** `{sessionId, toolCall, options:
  PermissionOption[]}` → we reply `{outcome:{outcome:"selected", optionId}}` or
  `{outcome:{outcome:"cancelled"}}`. **This is the elicitation hook.**
- `fs/read_text_file`, `fs/write_text_file`, `terminal/*` — callbacks the agent makes
  *if* we advertised those capabilities. **Recommendation: advertise NO fs/terminal
  capabilities in v1** so the agent uses its own file/exec path (its own sandbox
  governs it) and we stay a pure observer + permission gate. Advertising them makes
  elanus the executor — a much bigger surface. (A1 to confirm the agent tolerates a
  client with no fs capability.)

`PermissionOption.kind` ∈ `{allow_once, allow_always, reject_once, reject_always}`.
Tool `kind` ∈ `{read, edit, delete, move, search, execute, think, fetch, other}`.
`stopReason` ∈ `{end_turn, max_tokens, refusal, cancelled, ...}`.

### The hooks verdict — Tim's question, answered

**Does ACP buy deep-enough integration (e.g. hooks)? Partly. It buys observability,
permission, resume, and MCP — but NOT hooks, and NOT mid-turn context injection.**

| What elanus does today | ACP mechanism | Verdict |
|---|---|---|
| **Observability** — every message, thought, tool call+result, file write | `session/update` stream | ✅ **Full, best-in-class.** No log-sniffing; the agent tells us everything as it happens. |
| **Permission gating** — pause a risky action, ask the human | `session/request_permission` | ⚠️ **Partial.** Real (blocking, relayable), but **agent-initiated** (we can't force a prompt on a call it deemed safe) and **allow/reject-only** (we pick one of its options; we cannot rewrite the call or hand back a correction). |
| **Mid-turn context injection** — Claude `PreToolUse` `additionalContext`, opencode `prompt_async`: steer the model *during* a turn | — **none** — | ❌ **Not supported.** During a turn we can only send permission answers, fs/terminal callbacks, and `cancel`. To inject you must cancel + re-prompt — coarse, loses turn state. |
| **Resume / reattach** | `session/load` (capability-gated) | ✅ **Cold resume.** ❌ **Live reattach** — a co-located stdio agent dies when we disconnect (same limit as the codex app-server). |
| **MCP pass-through** | `mcpServers[]` at `session/new` | ✅ **Cleaner than today** — we hand servers in directly instead of editing the tool's config file. |
| **Edit-claims (sibling awareness)** | derived from `tool_call.locations` | ✅ We derive it; ACP needn't know. |

**Bottom line for "should ACP be preferred going forward":**
- For **capture + approval + resume + MCP** ACP is a clean win and less code — prefer it.
- The **one axis it loses** is mid-turn injection, where the Claude hook adapter stays
  strictly more capable. **But codex already can't inject mid-turn** (the per-harness
  injection spike found "Codex degrades"). So an ACP agent sits at
  **codex-app-server depth, generalized** — better than `codex exec`, on par with the
  app-server, below the Claude hook adapter only on the injection axis codex lacked.
- **Ruling to hand Fable:** prefer ACP for new agents; keep the Claude hook adapter as
  the one that injects; treat "redo codex/opencode over ACP" as a *later
  consolidation*, not part of this handoff.

---

## Wonky bits / decisions to confirm (up front)

1. **ACP has no hooks and no mid-turn injection** (table above). The headline.
   Everything else about ACP is a win; this is the ceiling.
2. **The agent's spawn command is DATA, not code.** The leverage is that the *adapter*
   is generic and the *only* per-agent difference is which command to run
   (`gemini --experimental-acp`, `goose acp`, `codex-acp`, …). That argv must live in
   the **manifest** and the launcher must hand it to the adapter. **Recommendation:
   add optional `command` + `args` (or an `[acp]` sub-table) to `HarnessDecl` and have
   the launcher stamp it into the child env** (e.g. `ELANUS_ACP_ARGV` as JSON). Then a
   new ACP agent is a six-line TOML, no new binary. The single most important call here.
3. **The permission reply references agent-defined option IDs, not fixed keywords.**
   You reply with the `optionId` of one of the sent `options[]` (an `allow_*`-kind for
   grant, a `reject_*`-kind — or `cancelled` — for deny). Fail-closed = a reject option
   / cancelled. Mirrors the codex gotcha where a wrong keyword silently fails closed —
   get the option-picking exactly right.
4. **Hold-the-socket; no exit-and-resume during a turn.** Same as the codex app-server
   (its wonky bit 2): the driver blocks in-process on the mailbox answer while the
   socket stays open, because a co-located stdio agent dies if we disconnect. Live
   reattach impossible; resume is a *cold* `session/load`.
5. **ACP streams chunks; codex settled items.** `codex_appserver_map_item` maps a
   settled `item/completed`; ACP sends `agent_message_chunk` deltas. The obs mapper
   must **buffer chunks per message and flush** at a tool-call boundary and at turn
   end. Decide: one settled `assistant/message` per flush (recommended, matches the
   existing vocabulary) vs deltas.
6. **ACP is headless-shaped — v1 is headless-only.** ACP is client/server where
   *elanus is the client*; the agent has no human terminal UI of its own (Zed is the
   reference GUI client). Bare `elanus code <acp-agent>` (TUI) has no honest
   interactive story. **Recommendation: `Mode::Tui` returns a clear error** ("the acp
   harness is headless-only; use `--headless`, or run the agent's own CLI directly").
   A passthrough terminal client is out of scope.
7. **Driver reuse — the maintenance-vs-risk call.** (a) extract
   `drive_codex_app_server` into a dialect-parameterized core shared by codex+ACP; or
   (b) share only the security-critical **elicitation relay** and write a *fresh* `acp`
   driver module cloning the codex loop's proven shape. **Recommendation: (b).** The
   codex approval path is load-bearing and security-sensitive, and `src/codeagent.rs`
   is under active edit by sibling sessions now — refactoring it risks destabilizing
   approvals and colliding with in-flight work. The framing+dispatch loop is ~100
   low-risk lines; duplicating is cheap. What we DON'T duplicate is the one hard,
   security-critical thing: the emit-ask → await-correlation → fail-closed-default
   relay, lifted into a dialect-neutral helper. Full consolidation (codex-app-server
   as just-another-ACP-dialect) is a clean follow-up once ACP is proven — exactly Tim's
   "possibly redo opencode/codex over ACP later."

---

## Package & config layout

- **One `acp` package**, seeded like the other stock harnesses (extend
  `STOCK_HARNESS_PACKAGES` / `seed_stock_harness_packages`, `src/initcmd.rs`). Its
  `bin/adapter` is a copy of a new `harness-acp` Cargo bin (a 4-line shell calling
  `run_acp_adapter(Ctx::from_env())`, like `src/bin/harness-codex.rs`).
- **The package declares one `[[harness]]` block per known ACP agent**, each carrying
  its spawn argv:

  ```toml
  [[harness]]
  name = "gemini"
  agent_noun = "gemini"
  run = "bin/adapter"
  command = "gemini"
  args = ["--experimental-acp"]

  [[harness]]
  name = "goose"
  agent_noun = "goose"
  run = "bin/adapter"
  command = "goose"
  args = ["acp"]

  [[harness]]
  name = "codex-acp"
  agent_noun = "codex"        # (decide: same noun as native codex, or distinct)
  run = "bin/adapter"
  command = "codex-acp"       # or command="npx", args=["-y","@zed-industries/codex-acp"]
  args = []
  ```

  All point `run` at the **same** adapter; `command`/`args` are the only difference.
  Adding the next agent is appending a block — no code, no binary. (One `acp` package
  with N blocks keeps it in one place; N per-agent packages also work — pick the
  former.) The exact spawn commands/flags are **A1 to verify** against the installed
  binaries.
- **`HarnessDecl` gains `command: Option<String>` + `args: Vec<String>`** (generic
  "the argv this adapter execs"; `deny_unknown_fields` means we add fields, not a
  free table). The launcher (`src/codeagent.rs:~3616`) stamps them into the child env
  (`ELANUS_ACP_ARGV`); `Ctx` gains an accessor. Keep it generic (not ACP-named) — it's
  useful for any exec-shaped adapter.

---

## Milestones

### A1 — Pin the wire + a scripted fake agent (the gate)
Verify every ACP name against the real schema at `agentclientprotocol.com` **and**
against one real agent's actual `initialize` handshake. Write a **scripted fake ACP
agent** (a small stdin/stdout script, or an in-process mock reusing the
`drive_codex_app_server` frame-source seam) that answers `initialize`, `session/new`,
and on `session/prompt` emits a few `session/update` notifications (a message chunk, a
`tool_call`, a `tool_call_update` completed) then returns a `stopReason`; and on one
path issues a `session/request_permission`.
**Acceptance:** a checked-in table of exact wire names (divergences from this draft
flagged); a unit test where the fake agent drives one full turn through the (stub)
driver.

### A2 — The `acp` adapter + driver loop (headless capture, no elicitation yet)
New `harness-acp` bin + an `acp` driver module: newline-framed reader → channel,
`(method, has_id)` dispatch, the `initialize → session/new → session/prompt` state
machine, `session/update` → `ctx.emit` projection (buffer message chunks, flush to
`assistant/message`; `tool_call`→`tool/<kind|name>/call`, `tool_call_update`(done)→
`tool/<name>/result`, `agent_thought_chunk`→`assistant/reasoning`, `plan`→
`session/plan`), `tool_call.locations` → `ctx.claim`, `ctx.record(sessionId)` +
`session/thread` obs, `stopReason` → `session/idle`, all stamped `fidelity:
"acp-live"`. Unmodeled server requests refused with JSON-RPC `-32601` (fail-closed,
copy the codex driver). Provider creds scrubbed; the agent launches with its own auth.
**Acceptance:** `elanus code <fake-agent> --headless "<task>"` yields a full obs trail
(`session/thread`, `assistant/message`, `tool/*/call`+`result`, `file/*` claims,
`session/idle`); fake-agent unit test passes; no `fs/terminal` capability advertised.

### A3 — Elicitation relay (the security-critical part)
Extract the codex approval relay into a **dialect-neutral helper**: emit the ask to
`in/human/<owner>` with a fresh correlation + deadline + fail-closed `default_action`,
lay the `approval/ask|answer|decision` obs trail, poll the correlation across the
mailbox topics, return `allow: bool` (reuse `codex_appserver_await_answer`'s matching
logic). The ACP driver maps `session/request_permission`'s `options[]` → the correct
`optionId`: an `allow_*` option on grant, a `reject_*` option (or
`{outcome:"cancelled"}`) on deny. Fail-closed on timeout, emit failure, and
malformed/empty answer.
**Acceptance:** the fake agent's permission path round-trips through `in/human/<owner>`
back to the correct `optionId`; a timeout applies the default (deny); an unknown
server request returns `-32601`; unit + e2e green.

### A4 — MCP pass-through + config-driven agent registry
Add `command`/`args` to `HarnessDecl`; launcher stamps `ELANUS_ACP_ARGV`; `Ctx`
exposes it; the adapter execs that argv. Seed the `acp` package with the known-agent
blocks. Thread the user's MCP registrations into `session/new`'s `mcpServers[]`
(stdio always; http/sse only if `initialize` advertised the capability) and record the
merged names on the `session/thread` obs (mirror `merged_mcp_server_names`,
`src/codeagent.rs:1762`). Note: passing MCP at `session/new` is *cleaner* than the
file-merge other adapters do — decide the source (a per-agent `[acp].mcp` list vs a
shared elanus registry).
**Acceptance:** adding an ACP agent is a manifest-only edit (drop a new block, run it,
no rebuild); `elanus code gemini --headless` execs `gemini --experimental-acp`; MCP
servers appear in `session/new` and on the obs.

### A5 — Real-agent validation, docs, honest residuals
Run end-to-end against **≥1 real installed ACP agent**. (I could not probe this
machine — implementer checks what's present: `gemini --experimental-acp`, `goose acp`,
or `codex-acp` / `npx -y @zed-industries/codex-acp`. If codex is installed, `codex-acp`
is the safest real target — it re-runs the already-validated codex through the ACP
path, proving parity with the native app-server driver.) Update the onboarding doc's
ACP section from "does not exist yet" to the recipe. Record the residuals plainly.
**Acceptance:** a real agent completes a captured turn with an approval round-trip;
onboarding doc updated; residuals listed (below).

### A6 (deferred) — `session/load` resume
`ctx.record(sessionId)` lands in A2, but the mailbox→resume daemon path (M2-B) drives
resume through `resume_command_for` — a **CLI** table (`src/codeagent.rs:7931`), and
ACP has no CLI resume. ACP resume must re-enter the driver in a "load" mode (spawn →
`initialize` → `session/load` → `session/prompt` the new message). That's a
driver-mode addition, not a table row. **Defer**; A2 records the id so nothing is lost.

---

## Validation strategy (both halves matter)
- **CI / deterministic:** the scripted fake ACP agent (A1). No network, no real agent;
  reuses the frame-source/sink seam that already makes `drive_codex_app_server`
  testable against a mock. Covers the loop, obs projection, the elicitation
  round-trip, the fail-closed timeout, and the `-32601` refusal.
- **Reality check:** ≥1 real agent (A5) — a fake agent can't catch a wrong capability
  flag or spec drift. `codex-acp` is the highest-confidence real target (closes the
  loop with an already-trusted engine).

## Residuals (name them, don't bury them)
- **No mid-turn context injection** — the one thing the Claude hook adapter does that
  ACP cannot. Acceptable because codex already couldn't.
- **Permission is allow/reject-only and agent-initiated** — we can't force a gate or
  rewrite a call, only answer the ones the agent raises.
- **Headless-only** — no TUI in v1 (wonky bit 6).
- **Resume deferred** to A6.
- **`fs/terminal` capabilities not advertised** — by choice, to stay observer + gate
  rather than executor. Revisit only if an agent *requires* client-side fs.
- **Spec names are a draft** until A1 pins them.
- **Full codex/opencode-over-ACP consolidation** is out of scope — a future win once
  ACP is proven, per Tim.

## Log
- 2026-07-03 — Handoff written (Claude Opus 4.8, planner). Grounded in the in-repo
  codex app-server driver + the pluggable-harness SDK (verified against source). ACP
  wire details are from training + `docs/coding-harness-onboarding.md`; the live spec
  could NOT be fetched (sandbox blocked curl/WebFetch/WebSearch in this non-interactive
  session), so A1 is the gate that pins them. Local ACP agents could not be probed
  (sandbox restricts paths to the workdir); A5 checks what's installed.
- 2026-07-06 — A1-A3 implementation pass (Codex). A1 fetched the live ACP v1 docs
  and schema release `schema-v1.19.0`, captured a real `codex-acp` initialize
  response, and checked the divergence table into `docs/acp-wire-notes.md`. A2/A3
  added `src/acp.rs` plus the `harness-acp` bin: newline JSON-RPC driver,
  initialize -> session/new -> session/prompt, chunk-buffered obs projection,
  fail-closed `-32601` for unmodeled server requests, and ACP permission
  elicitations relayed to `in/human/<owner>` with optionId mapping. A4-A6 remain
  deferred.
