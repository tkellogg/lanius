---
status: exploratory
author: Claude Opus 4.8 (planner)
last-updated: 2026-07-07
---

# Consolidating codex/opencode onto the ACP harness — a STAGED, GATED plan

**Read this first: this is not a green light.** The idea — retire the bespoke
codex/opencode adapters and drive them through the one generic ACP harness — is
directionally right and anticipated in `acp-harness.md` (residuals, "a future
win once ACP is proven"). But grounded research found it **premature today**,
with hard prerequisites and real losses. This handoff exists to (a) stop a naive
"just point the manifest at the ACP adapter" change that would silently break
resume and TUI, and (b) lay the staged path so it can be done safely later.

**Do not schedule M1 until the M0 gate is green.**

---

## The honest verdict (grounded)

Three blockers, each concrete:

1. **ACP resume (A6) does not exist yet.** Resume today is a hardcoded CLI table,
   `resume_command_for` (`src/codeagent.rs:7963`): `codex exec resume …`,
   `opencode run --session …`. Pointing a codex/opencode launch at the ACP
   adapter would leave resume trying that CLI against an ACP session id it can't
   use — **resume breaks**. A6 (`docs/handoffs/acp-session-load-resume.md`,
   `status: planned`) is a **hard prerequisite**, not optional.

2. **No verified opencode ACP bridge exists.** Across `acp-harness.md`,
   `acp-wire-notes.md`, and the 25+-agent ACP list in
   `coding-harness-onboarding.md`, **opencode never appears as ACP-capable.** The
   "opencode could run through ACP" premise has *no grounded engine to point at*.
   Treat opencode-over-ACP as a **research spike** ("does opencode speak ACP at
   all?"), not an implementation milestone.

3. **codex-acp is schema-verified but not host-validated.** A1 captured a real
   handshake against `@zed-industries/codex-acp@0.16.0` (which warns it's
   deprecated for `@agentclientprotocol/codex-acp`), but that package **is not
   installed on this host** — A5 validated against `goose` instead. So "codex
   through ACP" is unproven end-to-end here.

## What's gained vs lost (so the trade is explicit)

**Gained:**
- **Real permission gating for codex.** codex's *default* headless path today
  runs `approval_policy=never` + `sandbox_mode=danger-full-access`
  (`codeagent.rs:4299`) — it auto-approves everything, because tighter settings
  silently cancel MCP calls headless. ACP's `session/request_permission` relay
  (`src/acp.rs:743`) gives genuine elicitation. This is a real safety win.
- **Less duplicated stream-parsing** — the ACP driver's obs projection replaces
  `capture_codex_stream`/`capture_opencode_stream`'s per-engine JSON vocabularies.

**Lost / must be preserved (do NOT full-retire the native adapters):**
- **TUI.** `run_acp_adapter` hard-bails on `Mode::Tui` (`src/acp.rs:98`). The
  native `run_codex_tui_import` / opencode TUI paths are live interactive cells.
  Consolidation can only swap the **headless** path; the native adapters stay for
  TUI. This is a headless-path swap, **not a retirement.**
- **Resume** — until A6 ships (blocker 1).
- **MCP auto-merge.** codex/opencode transparently pick up the user's *existing*
  native MCP config (`merged_mcp_server_names`, `codeagent.rs:1763`). Under ACP,
  MCP servers must be **redeclared per-agent** in the manifest `[[harness]].mcp`
  block (A4's decision). A workflow change to flag, not free.
- **opencode's future mid-turn injection.** opencode's `prompt_async` mid-cycle
  push is spiked-but-deferred; ACP has no mid-turn injection, so moving opencode
  onto ACP **permanently forecloses** that. codex already can't do it, so no loss
  there.

## The dispatch-hardcoding wrinkle (the real refactor)

Launch is manifest-driven (`HarnessDecl.command/args/mcp`,
`STOCK_HARNESS_PACKAGES` already seeds a `harness-acp` package). But **resume,
approval/cage posture, and MCP introspection are hardcoded Rust `match` arms**
keyed on `harness_id_for_tool` (`codeagent.rs:380`) returning static
`"codex"`/`"opencode"` — see `resume_command_for` (`:7963`),
`resume_stream_capture_for` (`:8005`), `codex_headless_approval_posture`
(`:4330`), `codex_headless_cage_posture` (`:4353`). Simply re-pointing codex's
manifest block at the ACP adapter would leave all of these firing wrongly.

**Design choice (recommend): a distinct tool identity, not an overload.**
Introduce `codex-acp` as its OWN `harness_id_for_tool` arm / tool id, routed
through the ACP adapter + the A6 ACP resume path, rather than making the string
`"codex"` mean two different transports. Native `codex` stays untouched (TUI +
fallback). Users opt into the ACP-flavored one. This avoids threading a
"launched via ACP" flag through every posture/resume `match`.

---

## Milestones (staged; each gated on the prior)

### M0 — Prerequisite gate (no code; a go/no-go)
- **A6 (ACP resume) is shipped and green.**
- A real `@agentclientprotocol/codex-acp` (or `@zed-industries/codex-acp`)
  binary is installed and completes a captured turn **with an approval
  round-trip** through the existing ACP harness (the A5 recipe, against codex-acp
  this time, not goose).
- **Acceptance:** both true, written down. If either is false, STOP — do not
  start M1.

### M1 — `codex-acp` as a first-class ACP-driven identity
- Register `codex-acp` as its own tool id (new `harness_id_for_tool` arm) backed
  by the ACP adapter; seed/enable its `[[harness]]` block (command
  `codex-acp` / `npx -y @agentclientprotocol/codex-acp`, per-agent `mcp`).
- Route its resume through the A6 ACP `session/load` path (NOT the CLI table);
  give it an approval/cage posture appropriate to ACP (the relay gates, so it
  need not run `danger-full-access`).
- Leave native `codex` (TUI + the existing headless default) **fully intact** as
  the fallback.
- **Acceptance:** `lanius code codex-acp --headless "<task>"` runs a captured
  turn with real permission elicitation; resume continues it (A6); native
  `codex` behavior is byte-unchanged; `cargo test` green.

### M2 — opencode-over-ACP: a research spike (NOT an impl milestone)
- Determine whether opencode speaks ACP at all — a native mode, a wrapper, or
  nothing. Grounded finding only.
- **Acceptance:** a written verdict. If no bridge exists, opencode consolidation
  is **dropped** (documented); do not schedule M3.

### M3 — (conditional) consolidate opencode's headless path
- Only if M2 found a real bridge. Mirror M1 as a distinct `opencode-acp`
  identity; preserve native opencode TUI + the deferred `prompt_async` future by
  keeping the native adapter.
- **Acceptance:** as M1, for opencode; the `prompt_async` deferral is explicitly
  reconsidered before foreclosing it.

---

## Non-goals
- **Retiring the native codex/opencode adapters.** TUI needs them; keep them.
- **A big-bang swap of the `"codex"`/`"opencode"` identities.** New ACP-flavored
  identities instead — reversible, side-by-side, safe.

## Read these first
- `src/acp.rs` — the ACP driver: `run_acp_adapter:97` (TUI bail `:98`),
  `drive_acp_session:190`, `build_mcp_servers:640`, permission relay `:743`.
- `src/codeagent.rs` — the bespoke adapters: `run_codex_adapter:4149`,
  `run_opencode_adapter:4206`, `capture_codex_stream:7523`,
  `capture_opencode_stream:7146`; the hardcoded dispatch:
  `harness_id_for_tool:380`, `resume_command_for:7963`,
  `codex_headless_approval_posture:4330`, `codex_headless_cage_posture:4353`,
  `merged_mcp_server_names:1763`.
- `docs/handoffs/acp-harness.md` (A1–A5) + `acp-session-load-resume.md` (A6, the
  M0 prerequisite).
- `docs/coding-harness-onboarding.md` — the ACP agent list + the codex-acp note.

## Log
- 2026-07-07 (Opus, planner): wrote the handoff from grounded research. Verdict:
  **premature — gate on A6 + a host-validated codex-acp before any code.** Key
  calls: it's a headless-path swap, not a retirement (TUI keeps the native
  adapters); introduce `codex-acp` as a distinct tool identity rather than
  overloading `"codex"` across the hardcoded resume/posture `match` arms;
  opencode-over-ACP is a research spike (no bridge is known to exist), not a
  milestone. Real upside worth doing later: codex's default headless path
  auto-approves at `danger-full-access` today; ACP gives it genuine gating.
