---
status: planned
author: Claude Opus 4.8 (planner)
last-updated: 2026-07-07
---

# Helper M4 — harness-backed turns (run the helper on the CLI you already have)

**The promise** (journey 15, `docs/journeys/15-agentic-configuration.md:43-46`):
> "run this agent via headless calls to `lanius code {claude|codex|opencode}` if
> that's what they have available… as seamless as possible. And I don't want to
> cause an 'oh shit' moment if they don't want to do API billing."

The helper (agentic-configuration M1–M3) currently needs a **dispatcher-usable
provider** (an ApiKey) to run a turn. M4 makes it run through the user's
**already-logged-in coding CLI** when that's all they have — LLM-detection's
**world (b)**. This closes the "no dead ends" loop: M3 detects a harness, M4 uses
it.

This supersedes the M4 sketch in `agentic-configuration.md:186-217` with a
grounded seam design. **Scope: the helper profile only** (a general
per-profile harness fallback is a deferred follow-up).

---

## The spike is a GATE, not a formality (do this before building the seam)

M4.0 below is a real go/no-go. The mechanism has two unproven risks; if either
fails, **M4 stops and world (b) collapses into world (c)** (static setup-only,
no chat). Budget for that outcome.

1. **Latency.** A harness turn is a **blocking, single-shot** `resume_capture`
   (`src/codeagent.rs:8111`) — it returns one final answer after the whole CLI
   run exits (obs progress streams during the run, but the *answer* lands at the
   end). Is that acceptable for an interactive helper panel?
2. **The tool loop.** A native chat turn gives the model lanius's own tool defs
   (`shell`, `kb`, … via `exec.rs:1617 tool_defs`/`run_tool`). A harness turn
   gives the CLI **only a flat text injection** (`turn_injection`,
   `codeagent.rs:3461`) plus the CLI's **own** tool loop (bash/file-edit) and
   whatever MCP-on-launch merged. The helper's charter says its reads go through
   `shell` (`lanius status`, `config get`, …) — under the harness those must
   become **literal shell commands the CLI runs itself**, or an **`lanius` MCP
   server** exposed at launch. Proving the CLI can actually perform the helper's
   reads is the spike's crux.

---

## The seam (grounded design)

**Where a turn dies today.** Mailbox event `in/agent/helper` → `dispatch_pending`
(`src/dispatcher.rs:1701`) → the helper package's `scripts/run` → `handle_exec`
(`src/exec.rs:3685`) → `run_turn` → `dispatcher_plan` (`exec.rs:1298`) resolves
`[model].provider`. With no ApiKey provider it either `bail!`s (a NativeLogin
provider, `provider.rs:266`) or falls to the genai default and fails at the
network/auth call. Nothing detours to a harness.

**What M4 adds.** When the profile is `helper`, `[model] harness_fallback = true`
is set, and detection says world (b), **intercept before/at that provider
resolution** and route the turn into the headless-coding machinery instead:

- **First contact = LAUNCH, not resume.** There is no existing path that
  recognizes a *first* helper mailbox delivery and launches a session — the
  coding-worker loop's `recognize_delivery` (`codeagent.rs:523`) only matches a
  4-segment `in/agent/<tool>/<conv>` where `<conv>` is an **already-recorded**
  `codesession`. M4 must, on first contact, call `launch_external_harness`
  (`codeagent.rs:3471`) with **`agent_noun = "helper"`** and record a
  `SessionRecord` keyed to the helper conversation so the *next* turn resolves
  back to the same native session.
- **Subsequent turns = `resume_capture`** that same native session
  (`codeagent.rs:8111`), which already scrubs lanius's provider creds
  (`scrub_provider_creds`, `:8181`) so the CLI uses **its own login** — a
  correctness invariant to preserve.
- **Reply routing = reuse `route_completion`** (`dispatcher.rs:1061`): the
  worker's `final_text` routes to `in/human/<owner>` as a correlated event.

**The chat panel likely needs NO change.** `conversation_messages`
(`web.rs:2679`) reconstructs the panel from the **ledger** — the `in/agent/helper`
delivery (the "you" side, matched by `payload.session`) + the `in/human/<owner>`
reply (the "agent" side). If M4's first-turn delivery stays a normal
`in/agent/helper` event and its completion routes via `route_completion`, the
panel renders with no UI work. **Confirm empirically in the spike** rather than
assume.

**Context blocks come for free** — `turn_injection`'s `session_memory_blocks`
(`codeagent.rs:2984`) reads the same `context_blocks` table (agent-scope) that a
native turn renders, so the helper's charter/progress/kb blocks surface into the
harness turn **iff the launched session's `agent_noun` is `"helper"`**. Confirm
`agent_noun` is parametrizable per-launch (it reads
`external.decl.agent_noun`, `codeagent.rs:3490`).

---

## Milestones

### M4.0 — SPIKE (go/no-go gate; no seam code yet)
- By hand: run a helper-style turn via `lanius code claude --headless "<task>"`
  where `<task>` carries the helper's charter + a real ask ("is my setup
  healthy?"). Measure end-to-end latency. Verify the CLI can perform the
  helper's **reads** (`lanius status`, `config get`, …) — via its own shell,
  and/or decide the `lanius` MCP-server-at-launch approach.
- Verify **fail-closed**: an unauthed / killed CLI produces the **failure-mail
  contract** (`{failed:true}` on the correlation), not a hang. (world (b) means
  "binary on PATH", NOT "logged in" — `web.rs:562`.)
- **Acceptance:** a written go/no-go. Latency + tool-loop acceptable → proceed.
  Otherwise STOP, document why, and make world (b) fall through to world (c).

### M4.1 — the opt-in + detection wiring
- Add `harness_fallback: bool` to `profile::ModelCfg` (`profile.rs:99-113`),
  default false; set it in the helper profile. Wire world-(b) detection
  (reuse M3's `llm_detection`, `web.rs`) as the runtime condition.
- **Acceptance:** a helper profile with `harness_fallback=true` + world (b) is
  recognized as "route to harness"; every other profile is unaffected;
  `cargo test` green.

### M4.2 — the routing seam
- Intercept the no-usable-provider helper turn before/at `dispatcher_plan`;
  first contact → `launch_external_harness(agent_noun="helper", …)` + record the
  `SessionRecord`; subsequent turns → `resume_capture`; route completion via
  `route_completion` to `in/human/<owner>`.
- **Acceptance:** a helper mailbox turn in world (b) launches a headless session,
  the reply lands in the chat panel (no UI change), and a second turn resumes the
  **same** native session (verified via the `codesession` record + obs).

### M4.3 — the tool-loop bridge (as the spike settled it)
- Implement whichever the spike proved: the helper's reads run as literal CLI
  shell commands (a skill/kb tells the model the `lanius` commands), and/or an
  `lanius` MCP server exposed via MCP-on-launch so the CLI calls `status`/
  `config get`/… as MCP tools.
- **Acceptance:** in a harness-backed turn the helper actually answers a "is my
  setup healthy?"-class question using real read data, not a hallucination.

### M4.4 — fail-closed + validation
- The failure-mail contract fires on an unauthed/killed/timed-out CLI (reuse the
  resume timeout + failure-mail protocol); the panel shows an honest error, not a
  hang.
- **Acceptance:** kill the CLI mid-turn → the panel gets `{failed:true}`, not a
  spinner forever; docs updated; residuals named.

---

## Wonky bits / decisions
1. **Tool-loop translation is the real design fork** (M4.3): CLI-shell + a
   guiding skill, vs. an `lanius` MCP server at launch. The spike picks it. The
   MCP route is cleaner (typed tools) but heavier; the shell route is lighter but
   leans on the model doing the right `lanius …` calls.
2. **world (b) is PATH-only, not login-verified** — so M4 MUST fail closed on an
   unauthed CLI. Do not treat "binary present" as "usable."
3. **Preserve the cred-scrub invariant** — the harness turn uses the CLI's own
   login; never leak lanius's dispatcher creds into it (`scrub_provider_creds`).
4. **`agent_noun="helper"` on the launched session** — required so context blocks
   and obs land under the helper identity.
5. **Repo drift note:** the old M4 sketch cites `resume_capture` at `:7968`; it's
   now `codeagent.rs:8111`. Re-anchor when implementing.

## Non-goals / residuals
- **General per-profile harness fallback** — deferred follow-up
  (`agentic-configuration.md:239`); this is helper-only.
- **Login probing** — a cheap deterministic "is the CLI logged in?" check would
  make world (b) honest; out of scope, noted.
- **Streaming the final answer** — blocking single-shot is accepted; live obs
  gives progress, the answer lands at end. Revisit only if latency fails M4.0.

## Read these first
- `docs/journeys/15-agentic-configuration.md` + `docs/handoffs/agentic-configuration.md`
  (M4 sketch `:186-217`, wonky bit 7 `:82`).
- `src/exec.rs` — `handle_exec:3685`, `run_turn`, `dispatcher_plan:1298`,
  `tool_defs:1617` (the native tool loop M4 replaces).
- `src/codeagent.rs` — `launch_external_harness:3471`, `resume_capture:8111`,
  `recognize_delivery:523`, `turn_injection:3461`,
  `session_memory_blocks:2984`, `scrub_provider_creds:8181`.
- `src/dispatcher.rs` — `dispatch_pending:1701`, `drive_code_deliveries:1388`,
  `route_completion:1061`.
- `src/web.rs` — `llm_detection` (world a/b/c), `conversation_messages:2679`.

## Log
- 2026-07-07 (Opus, planner): grounded the M4 sketch into a seam design. Key
  findings: first contact must LAUNCH (no path recognizes a first helper mailbox
  delivery); the chat panel is ledger-reconstructed so likely needs no change;
  context blocks surface for free if `agent_noun="helper"`. The spike (M4.0) is a
  real go/no-go — latency + the tool-loop translation (helper's `shell` reads →
  CLI shell or an `lanius` MCP server) are the two risks that can sink M4 into a
  world-(c) fallback. Coupled to the dispatcher tool-call-liveness handoff (a
  harness-backed turn that relays tool calls through the ledger hits that gap).
