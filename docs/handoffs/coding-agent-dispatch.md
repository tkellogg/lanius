---
status: done
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-22
---

# Handoff: reliable worker dispatch from inside a coding session

A work plan to close the seam between a coding session and elanus's
orchestration machinery — the part an agent actually touches when it is told
"dispatch a worker." The machinery underneath (per-session identity, the
hook→bus record, the mailbox, the daemon resume loop) already works; what is
missing is a **discoverable, honest, footgun-free front door** to it. This
handoff is the remedy for the failures recorded first-hand in
[../journeys/08-dispatching-a-worker.md](../journeys/08-dispatching-a-worker.md).

## Read these first

- [../journeys/08-dispatching-a-worker.md](../journeys/08-dispatching-a-worker.md)
  — the lived failure this plan fixes (the *why*).
- [coding-agents.md](coding-agents.md) — the parent envelope design (one
  envelope, two adapters) and its M0–M5 Log. This handoff is a follow-on, not a
  replacement: it changes the agent-facing surface, not the cage/record/identity
  core.
- [../../src/codeagent.rs](../../src/codeagent.rs) — `briefing()`,
  `take_brief_flag`, `launch`, `run_codex_capture`, `deliver`, `inbox_cmd`,
  `turn_injection`, `CaptureSummary`. Everything below edits this file.
- [../../src/main.rs](../../src/main.rs) — the `Cmd::Code { tool, args }` match
  (the verb router with no help branch).

## The problem in one paragraph

The dispatch capability is reachable only as (a) prose baked into the launched
session's system prompt and (b) a CLI with no help text. The briefing documents
`deliver` (which needs an already-running worker) but not the launch verb that
creates one; the launch verb was discoverable only by triggering the
`unknown coding tool` error. A fresh launch **blocks** the caller and routes its
result to the bus, while `deliver` is **async** and routes back to the mailbox —
two execution models behind one word "dispatch," with no signpost. Worst, the
most natural launch invocation, `echo prompt | elanus code codex`, silently drops
the prompt: the launcher writes the *briefing* to codex's stdin and expects the
prompt as a positional arg (`run_codex_capture`). The net effect: an agent that
is the orchestration's intended primary user cannot use it without first breaking
it, and can fail while believing it succeeded.

## Design stance

Match the project's grain (see [elanus-conventions], CLAUDE.md): plumbing +
record over new authority; the agent reads context rather than speaks a protocol;
no home-state pollution; fail closed and *loud*, never silent. Nothing here adds
a trust boundary between the user's own agents — planner and worker remain
homogeneous authority (docs/security.md). The fixes are ergonomics, honesty, and
one missing verb, not a new permission model.

---

## D0 — Tell the truth in the briefing (cheapest, highest leverage)

The briefing is the only interface most sessions will ever read. Make it
complete and correct.

- Extend `briefing()` to document the **launch/spawn** verb alongside `deliver`,
  and to distinguish the two execution models in one sentence each: a fresh
  launch runs the worker now and reports to the bus; `deliver`/spawn is the
  async, wake-me-when-done path that routes back to your mailbox.
- State the **prompt-is-an-argument** rule explicitly for codex
  (`elanus code codex "<prompt>"`), and that piping into the launcher does not
  set the prompt today.
- Point at a real front door: "run `elanus code help` to see every verb."

**Acceptance:** a session, reading only its briefing, can spawn a worker with a
prompt that actually arrives, and knows which dispatch gets a result back.

## D1 — Give the CLI a front door

- Add `elanus code help` (and make `elanus code --help` / `elanus code` with no
  args print it, instead of a clap error) listing every verb — `claude`,
  `codex`, `deliver`, `inbox`, `resume`, `note`, `claim`, `unclaim`, `claims`,
  `spawn` — each with a one-line usage. Today these are invisible unless already
  known (`main.rs` `Cmd::Code` match).
- Make `elanus code list` print the supported tools instead of falling through
  to `launch("list")` and erroring. Discovery should never require triggering a
  failure.

**Acceptance:** the first thing a capable agent tries (ask the tool what it can
do) succeeds and is sufficient to dispatch correctly.

## D2 — Make launch unable to silently eat its input

Two independent footguns in `run_codex_capture` / `launch`:

- **Forward stdin, or refuse without a prompt.** Either (preferred) forward the
  launcher's stdin to codex when no positional prompt is given — so
  `echo prompt | elanus code codex` works the obvious way — or validate that a
  prompt is present (positional or stdin) and **error loudly** if not. The
  current behavior (briefing-to-stdin + dropped caller stdin + no prompt =
  briefing-as-prompt) must become impossible.
- **No session on an empty prompt.** `elanus code codex` with no prompt at all
  must not mint an identity and spawn a real run (the stray `code-6e1daf06` from
  the journey). Validate *before* `codesession::mint`.

**Acceptance:** every code path either delivers the caller's prompt to the worker
or fails with a clear message; no invocation starts a worker with an empty or
substituted prompt.

## Two dispatch modes (= the drive-pattern axis) — and the briefing must teach both

> **Axis note (2026-06-20; HM3/HM4 landed 2026-06-22):** the "two dispatch modes"
> here ARE the **drive-pattern** axis of [harness-modes.md](harness-modes.md) —
> *live/blocking one-shot* (the caller waits and reads the result inline) vs *async*
> (the daemon resumes the caller later via `spawn`/`deliver`/`resume`). That is
> distinct from the **launch-mode** axis (a harness running as an interactive `tui`
> vs a `headless` process), whose canonical, harness-uniform model is harness-modes.md.
> Both compose: drive pattern (blocking/async) is *how the result returns*; launch
> mode (tui/headless) is *how the process runs*. The uniform `--headless` flag and the
> bare-`elanus code <h>` → TUI behavior in harness-modes.md **superseded** the per-tool
> invocation quirks this handoff assumed (codex positional = headless, claude
> `--worker`): every harness now takes `--headless` (`--worker` a deprecated alias) and
> a bare/prompt invocation opens that harness's TUI. As of HM4 the `briefing()` teaches
> **both** axes by these names; read it and harness-modes.md for the live model — the
> per-tool blocking instructions below are the original (now-superseded) framing, kept
> for the D-milestone history.

- **Blocking foreground (a LIVE orchestrator).** A caller that is itself driven
  turn-by-turn — a human in a TUI, or a tool-using agent like Claude Code running
  Bash — should run the worker as a **blocking command** and read its result as
  the command's own output. The return *is* the trigger: a blocking
  `elanus code codex "<task>"` finishes, prints the worker's result, and that
  output resumes the caller in the same turn. No mailbox, no daemon, no
  "end your turn" — strictly simpler, and the correct default when hands are on.
  This is the model Tim wants for the TUI (see the journey's *Tim's perspective*):
  run a subagent, get the answer back inline.
- **Async spawn/deliver (a HEADLESS planner).** A caller grinding on its own ends
  its turn after dispatch and is woken when the worker reports back via its
  mailbox (the daemon resume loop). This is `deliver`/`spawn` (D3).

**Per-tool wrinkle for the blocking mode.** The two adapters reach "blocking
command that returns a result" differently, so the briefing's instructions must
differ by tool:

- **Codex** is already one-shot: `codex exec` runs to completion and the launcher
  blocks (`run_codex_capture`'s `child.wait()`). The prompt is a **positional
  arg** (`elanus code codex "<task>"`), and D2 must ensure it actually arrives and
  D4 must print the captured result to stdout so the caller sees it.
- **Claude Code** is launched today as the **interactive TUI** (inherited stdio),
  which does not return a clean machine-readable result. For a blocking
  *worker* invocation it should use headless print mode (`claude -p "<task>"`),
  not the TUI — a separate launch shape the briefing names explicitly.

**Acceptance:** the briefing tells a caller which mode to use by whether it is
live or headless, and gives the exact per-tool blocking invocation; a live
Claude Code session can run `elanus code codex "<task>"`, block, and read the
worker's answer as the command output.

## D3 — One async spawn verb (the verb "dispatch" should mean) — BUILT

> **Status (2026-06-20): built.** Implemented NOT via the daemon-driven
> "create-record-without-running" primitive (deferred as too invasive) but via a
> simpler, self-contained design that reuses the existing routing: a **detached
> self-relaunch + reply-on-completion**. `spawn()` (run inside the spawner
> session) pre-mints the worker id, then starts a *detached* background
> `elanus code <tool> [--worker] <prompt>` with `ELANUS_CODE_FORCE_SESSION`
> (the worker id, honored by `launch_session_id`), `ELANUS_CODE_REPLY_TO` (the
> spawner), and `ELANUS_CODE_REPLY_CORRELATION`, scrubbing the spawner's own
> identity env so the worker mints its own credential. It returns immediately with
> the handle. When the detached worker finishes, `launch()` best-effort emits a
> completion delivery (worker result: final text + files) to the spawner's mailbox
> via the SAFE `mailbox_for_actor` resolver + `events::emit` (sender = worker), so
> the existing M2-B resume wakes the spawner — closing the loop with no new bus
> authority.
>
> **Adversarially reviewed (two Claude-subagent passes).** No Critical/High
> security issues (confused-deputy routing, identity/credential scrubbing, and
> control-var bleed are correct). Fixes applied for the issues found: a worker
> **timeout** (detached workers only — `ELANUS_CODE_SPAWN_TIMEOUT_SECS`, default
> 1800 — so a hung tool still wakes the spawner once), a **fork-bomb depth guard**
> (`ELANUS_CODE_SPAWN_DEPTH` ≤ `MAX_SPAWN_DEPTH` = 8, propagated to nested spawns),
> a **FORCE_SESSION token-clobber guard** (`launch_session_id` refuses a forced id
> whose credential already exists; safe because `reap_orphans` runs first), and a
> capped completion file list. The parent→child edge (D4b) is preserved for
> spawned workers via the `ENV_REPLY_TO` fallback in the parent capture.
>
> **Residual (lower-severity, not yet fixed):** orphaned `run_dir()/<session>`
> scratch dirs are not crash-swept (only the token is reaped — litter, not a
> credential leak); `process_group(0)` detachment is `cfg(unix)`-only; live
> end-to-end daemon-resume-of-the-spawner across a real run is unit-tested but not
> integration-tested (and a guard against headlessly resuming a *live interactive*
> spawner is worth a look).

Today async dispatch (`deliver`) presupposes a worker, and creating a worker
(`launch`) is synchronous — a chicken-and-egg that forces either blocking or a
manual background `&`. Add `elanus code spawn <tool> "<prompt>"`:

- Launches the worker **detached** (background), prints its `code-<id>` handle
  immediately, and registers the spawning session as the requester so the
  worker's completion routes back to the spawner's mailbox — reusing the exact
  `deliver` → daemon-resume → `delivery_requester` machinery (`codeagent.rs`),
  no new authority.
- The spawner does precisely what the briefing already preaches: end its turn,
  get woken when the worker reports. This makes "dispatch then end my turn" true
  for *creating* a worker, not just for messaging an existing one.

**Acceptance:** `elanus code spawn codex "<task>"` returns a handle without
blocking; the spawner is resumed with the worker's verbatim result when it
finishes.

## D4 — Result visibility in band

`CaptureSummary` already harvests the worker's verbatim `final_text` and
`file_changes` (`capture_codex_stream`). Use it at the agent-facing edge:

- A blocking launch should print that summary to the caller on exit (not only
  publish obs the caller may not be able to read — a session's bus token is
  emit-only).
- A `spawn`/`deliver` completion already routes the summary to the mailbox via
  the daemon; confirm the round-trip carries `final_text` + `file_changes`.

**Acceptance:** a launcher/spawner sees the worker's actual answer without
needing bus read authority.

## D3b — A headless Claude launch shape (`claude -p`)

The blocking-foreground mode (above) needs Claude Code launched as a **headless
worker**, not the interactive TUI the launcher uses today (`launch`'s
`Capture::HookBridge` arm runs `claude` with inherited stdio). Add a worker/print
launch shape that runs `claude -p "<task>"` (headless print mode), captures its
result the way codex's `exec` path does, and prints it to stdout — so a parent can
`elanus code claude --worker "<task>"` (or via D3 `spawn`) and read the answer as
the command output. The interactive TUI shape stays the default for a human-driven
session; the headless shape is for a worker driven by another agent.

**Acceptance:** a parent can launch a Claude *worker* non-interactively and
receive its final result as the command's stdout, symmetric with codex.

## D4b — Capture enough to render the session tree (prerequisite for observability)

The human-facing observability track (see *Companion track* below, and Tim's
perspective in the journey) can only show what the record contains. Three fields
the obs/ledger does **not** capture today must be added at the source so the web
UI — and an explainer agent — have them:

- **Model + effort level.** `session/start` records the tool and args but not the
  resolved model or reasoning effort (`codeagent.rs`, `publish_obs(... "session/start" ...)`).
  Extract them: for codex from the `thread.started`/`turn` metadata in the JSONL
  stream; for CC from the launch args / hook payload. Land them on `session/start`
  (and on resume) so every session carries "what brain, what effort."
- **A "resumed" marker.** A resume (`elanus code resume`) reuses the same session
  leaf with no flag distinguishing it from a fresh turn. Emit an explicit
  `session/resume` (or a `resumed: true` on the turn record) so the UI can badge
  it and count resumes.
- **An explicit parent→child edge.** The spawner↔worker link is *derivable* from
  the delivery's `correlation` + `sender`, but the UI should not have to infer it.
  When `spawn` (D3) or `deliver` launches/drives a worker, record the parent
  session id on the worker's `session/start` so the tree is a direct read, not a
  join.

**Acceptance:** from the ledger alone, one can list every coding session with its
tool, model, effort, duration (start→stop), token usage, resume count, and parent
session — no field inferred, nothing missing.

## D5 — A discoverable skill / per-turn nudge (optional, after D0–D4)

Once the briefing and CLI are honest, promote dispatch from "prose in a system
prompt" toward a capability the agent is reminded of exactly when it needs it.

- **Per-turn nudge — BUILT (2026-06-20).** The CC hook bridge's
  `UserPromptSubmit` injection seam (`hook()` in `codeagent.rs`) now appends a
  one-line `DISPATCH_HINT` to the `[elanus]` `additionalContext` whenever the
  user's prompt mentions dispatch/delegation terms
  (`user_prompt_mentions_dispatch`: subagent, delegate, dispatch, spawn, worker,
  in parallel, codex, …). It composes with the existing inbox/note/peer-claim
  injection and stays silent on a quiet, non-dispatch turn — so the front door
  (`elanus code help`, the blocking `elanus code codex "<task>"` /
  `claude --worker` forms) surfaces on the exact turn the agent is asked to
  delegate. This directly closes the journey's failure (the human said "dispatch
  a codex subagent" and the agent had no front door). Note codex has **no** hooks
  (stream-parsed), so for codex the briefing carries this — the injection seam is
  CC-only.
- **Generated skill — BUILT (2026-06-20).** The research resolved cleanly:
  Claude Code loads `.claude/skills/` from any directory passed via `--add-dir`,
  and `--add-dir` is an explicit CLI flag that works even under
  `--setting-sources ''` (which only disables user/project/local *settings*
  discovery). So `launch()` (Claude path) writes an `/elanus` skill (a `SKILL.md`
  with the dispatch cheatsheet) to `<scratch>/skillroot/.claude/skills/elanus/` and
  passes `--add-dir <scratch>/skillroot` — session-scoped, no repo or `~/.claude`
  pollution, cleaned up with the scratch. The skill is placed in a dedicated
  `skillroot` subdir (not the scratch root) so the `--add-dir` grant does not also
  expose the generated `settings.json` (review finding M2). Codex has no skills, so
  its briefing carries the cheatsheet.

**Acceptance:** an agent learns dispatch exists without having to already know it
— met by the briefing (D0) + `elanus code help` (D1) + the per-turn nudge + the
invocable `/elanus` skill.

---

## Companion track — human-facing observability (Tim's perspective)

The journey now carries Tim's own ideal (see its *Tim's perspective* section): run
Claude Code in the TUI via `elanus code claude`, and have the web UI **auto-show**
that session — the session you're in, a paste-able resume command, basic stats —
and, nested under it, every subagent it spawned, with the same card per child:
which tool (codex? claude?), model + effort, how long it took, whether it was
resumed.

A separate **chat/explainer agent** (need not be Claude) that narrates what a
subagent did is **explicitly out of scope for now** (Tim, 2026-06-20) — but it
must remain *possible*. The only requirement it places on this work: the
observability data must be exposed through an **API / data hook** (a queryable
session + its full obs subtree), not locked inside the web UI, so an explainer
can be built later against the same surface with no rework.

This is a *different axis* from D0–D5: those make dispatch reliable from the
**agent's** seat; this makes the running tree legible from the **human's** seat.
They meet at the record — which is why **D4b is its hard prerequisite** (the UI
can't show model/effort/resumed/parent if they're never captured). The substrate
otherwise largely exists: obs leaves on the bus, the durable `code_sessions`
record (native id + tool + workdir for the resume command), token `usage` on
`session/idle`, and the `correlation`/`sender` edge for nesting. The missing
pieces are (1) D4b's capture gaps and (2) the web UI itself — `App.tsx` today has
only a generic `obs/agent/<agent>/#` telemetry scope, no coding-session list, no
tree, no resume surfacing.

**This track deserves its own handoff** (`coding-agent-observability.md`), backed
by the journey's *Tim's perspective* section, with its own milestones (a sessions
list → a session detail with resume command + stats → the nested subagent tree →
the explainer agent). It is recorded here as a pointer so the dispatch work
(especially D3 `spawn` and D4b capture) is built parent-link-aware from the start
rather than retrofitted. Written: see
[coding-agent-observability.md](coding-agent-observability.md).

## Are hooks wired in? (answering the design question)

Yes, for Claude Code: the launcher generates an isolated `--settings` config
wiring `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`,
and `SessionEnd` to `elanus code hook <event>` (`claude_settings` in
`codeagent.rs`). Those hooks do two jobs today: write the ordered record to the
bus, and on `UserPromptSubmit`/`SessionStart` inject the `[elanus]` per-turn
context block (inbox status, memory note, peer edit-claims) via `turn_injection`.
**Codex has no hooks** — its activity is captured by parsing the
`codex exec --json` stdout stream in-process.

So the hook machinery exists and is the right home for D5's per-turn nudge — but
today it carries inbox/notes/claims, never "here is how to dispatch." The
capability gap is not missing hooks; it is that the wired hooks were never asked
to teach the agent its own controls.

## Log

- 2026-06-20 — Written from the first lived dispatch failure
  ([../journeys/08-dispatching-a-worker.md](../journeys/08-dispatching-a-worker.md)).
  Root causes, all in `src/codeagent.rs` / `src/main.rs`: (1) `briefing()` omits
  the launch verb and the sync/async distinction; (2) no CLI help — verbs are
  discoverable only via the `unknown coding tool` error; (3) `run_codex_capture`
  consumes the launcher's stdin for the briefing and expects the prompt as a
  positional arg, silently substituting the briefing as the prompt when none is
  given; (4) a fresh launch blocks and reports only to the bus, contradicting the
  briefed "dispatch then end your turn" async model, which is `deliver`'s. None
  require changes below the agent-facing seam.
- 2026-06-20 — Tim added his perspective to the journey (human-facing
  observability: the web UI auto-showing the session + its subagent tree with
  per-child stats, a paste-able resume command, and an explainer agent). Adjusted
  this plan: added **D4b** (capture model/effort/resumed/parent so the tree is
  renderable — a prerequisite the dispatch work must honor now, not retrofit) and
  a **Companion track** pointer for a separate `coding-agent-observability.md`
  handoff. Grounding check confirmed the substrate mostly exists (obs leaves,
  durable record, token usage, correlation/sender edge); the gaps are D4b's three
  fields and the web UI view itself (`App.tsx` has no coding-session surface).
  The observability handoff is NOT yet written — pending Tim's go-ahead on scope.
- 2026-06-20 — Implemented D0/D1/D2/D4/D4b + the two-modes briefing, D3b headless
  Claude worker, and the D5 per-turn nudge via codex delegation (reviewed each
  diff, built, tested live). Then implemented the two remaining items at Tim's
  request: **D3 `spawn`** (codex xhigh effort) and the **D5 `/elanus` skill**
  (codex high effort). Process notes for future delegations: (a) codex's first
  spawn run silently ran `cargo fmt` across 24 files and under-reported its
  footprint — reverted the 21 fmt-only files; subsequent prompts added an explicit
  "do NOT run cargo fmt / edit only these files" guard and it complied. (b) My own
  review caught that `spawn` dropped the D4b parent edge (ENV_SESSION is scrubbed
  for the detached worker) — fixed via an `ENV_REPLY_TO` fallback in the parent
  capture. (c) Two adversarial Claude-subagent review passes (find → fix → verify)
  found no Critical/High security issues but two must-fix HIGHs (no worker timeout;
  no fork-bomb guard) plus mediums; all fixed and re-verified. All verified live
  against the production root `~/.elanus/root` with the daemon up (see
  [../runtime.md](../runtime.md)). cargo test: 171 passing.
