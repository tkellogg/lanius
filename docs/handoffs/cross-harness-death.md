---
status: planned
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: a dead worker always tells its spawner — for all three harnesses

The failure-mail contract exists and is honest on the **daemon-driven** path:
`settle_code_deliveries` (`src/dispatcher.rs:962`) turns a worker's outcome
into `failed = !matches!(done.success, Some(true))` (`:965-966`) and routes
`{failed:true}` mail on the requester's correlation (`route_completion`
`:1049`, payload `"failed": failed` at `:1136`, threaded at `:1149`). And —
better than expected from grounding — that path is **already harness-uniform**:
claude/codex/opencode all funnel through one `resume_capture`
(`src/codeagent.rs:6366`) with a single `child.wait()` (`:6480`) and
`success = status.success()` (`:6491-6493`), so a SIGKILL'd or nonzero-exit
*harness child* produces failure-mail on all three, and a headless **parent**
of any harness is woken by the same `drive_code_deliveries` →
`enqueue_code_job` → `resume_capture` machinery (`dispatcher.rs:1272`,
`:1449`; resume commands exist for all three: `codeagent.rs:6222/:6234/:6245`).

**The real hole is the detached async path.** `elanus code spawn`
(`codeagent.rs:952`) fires-and-forgets a detached process (`cmd.spawn()`
`:1015`); the worker mails back only from *inside itself* after its harness
child exits (`:3196-3198` wait → `emit_completion_delivery` `:3228`). Two
gaps: (1) if the detached worker **process itself** is SIGKILL'd before that
line, no mail is ever sent and nothing reaps it — `reconcile_lost_routes`
(`dispatcher.rs:1193`) covers only the durable driven path — the spawner hangs
forever; (2) even when it does mail, the payload is `{"prompt": …}` only
(`:1116`) with a textual `Status:` line (`completion_delivery_prompt`
`:1031`, `:1040-1044`) — **no structured `{failed:true}`**, so a planner (or
the UI) can't machine-read the failure the way the driven path allows. This
handoff closes the detached hole, proves the matrix, and writes the honest
per-harness wake table.

## Wonky bits / decisions to confirm

1. **Close the SIGKILL gap with a ledger heartbeat + reaper, not a supervisor
   process.** The detached worker is *deliberately* unparented (that's the
   async drive pattern), so nothing can `wait()` on it. But it already has a
   durable footprint: the `code_sessions` record and `last_active` (bumped
   per hook event, `codeagent.rs:6766-6770`), plus its recorded pid and the
   existing orphan machinery (`reap_orphans`/`reap_dead_members`, run at
   every launch, `codeagent.rs:3269-3276`). The fix: at spawn, record the
   spawn edge durably (spawner session/`reply_to`, correlation, worker
   session/pid) — the detached analog of `code_delivery_keys`; a daemon
   sweep (beside `settle_code_deliveries` in `tick()`, `dispatcher.rs:256-261`)
   notices a recorded detached worker whose pid is gone without a completion
   delivery, and emits the same failure-mail `route_completion` would have
   ("worker died without reporting"), settling the edge. Same
   ledger-durability pattern as every other sweep; survives daemon restarts.
   *Fable: confirm reaper-sweep over (a) a wrapper process that waits on the
   worker (changes the detach semantics) or (b) doing nothing and documenting
   the hang (fails the journey's whole point).*

2. **Give the detached completion mail the same structured contract as the
   driven path.** Add `failed: bool` (and the exit facts) as structured
   payload fields on `emit_completion_delivery` (`codeagent.rs:1094-1116`),
   keeping the human-readable `Status:` prompt line as-is. One contract,
   two producers — a consumer must not have to parse prose to learn a worker
   died. The reaper of wonky bit 1 emits the identical shape. *Fable:
   confirm payload-field addition (additive, no consumer breaks — the
   existing consumer reads `prompt` only).*

3. **The parent-wake table is better than the task assumed — verify, then
   document, don't rebuild.** Grounding refutes "headless claude resumes;
   what about codex/opencode": the daemon resume loop is harness-generic
   (single `resume_capture` dispatching on `rec.tool`). What genuinely
   differs per harness is the **capture quality and injection vector** on
   wake (codex mid-cycle degrades, etc. — the matrix at
   `codeagent.rs:2533-2537` is injection, not wake) and the **interactive
   TUI** case, where no harness can be woken (we don't own their event
   loops) and the inbox-pull pattern (`inbox_for_session`,
   `src/codesession.rs:474` + per-turn injection surfacing "N messages
   waiting") is the honest answer. M3's table documents *verified* behavior
   per (harness × mode), each cell backed by an M2 matrix run — not
   aspirations. *Fable: confirm verify-then-document scope for wake.*

4. **Explicitly OUT of scope: universal any-time-message-wake.** A TUI
   session blocked on user input cannot be made to act on arriving mail —
   its event loop belongs to the harness vendor, not elanus; the spike-proven
   partial exceptions (claude Pre/PostToolUse mid-cycle injection *while a
   turn is running*; opencode's server `prompt_async`) fire only during
   activity, not at rest. Chasing "always wake" means forking vendors'
   binaries or polling keystrokes — rejected. The table says this plainly per
   cell and points to inbox-pull. *Fable: confirm this stays a documented
   boundary, not a milestone.*

5. **The kill matrix must kill the right thing, twice.** "A worker killed
   mid-run" is two distinct deaths with different code paths: killing the
   **harness child** (the `claude`/`codex`/`opencode` process — the driven
   path's `child.wait()` sees it; the detached path's own wait sees it) and
   killing the **worker wrapper** (the detached `elanus code` process — the
   wonky-bit-1 hole; on the driven path the analog is killing the daemon
   mid-drive, which the existing boot re-pend + `reconcile_lost_routes`
   should already recover — verify). M2 tests all of it explicitly rather
   than assuming one kill covers both.

## Milestones

### M1 — Structured failure on the detached path + the spawn edge record
- `emit_completion_delivery` (`codeagent.rs:1094`) carries `failed`,
  `exit_code`, and the worker session id as payload fields beside `prompt`
  (wonky bit 2); `completion_delivery_prompt`'s prose unchanged.
- At `spawn` (`codeagent.rs:952-1015`): record the durable spawn edge
  (spawner/`ENV_REPLY_TO`, correlation, worker session, pid) before the
  detach; the worker marks it settled when `emit_completion_delivery`
  succeeds.

**Acceptance:** a spawned worker that exits nonzero delivers mail whose
payload has `failed: true` + the exit code (unit test on the payload shape;
existing prompt-text tests unchanged); a clean worker delivers
`failed: false`; the spawn edge row exists during the run and is settled
after delivery. `cargo test` green.

### M2 — The death matrix, proven for all three harnesses
The reaper sweep (wonky bit 1) in the dispatcher tick: unsettled spawn edges
whose worker pid is dead → synthesize the failure-mail (same structured
shape, `failed: true`, reason "worker terminated without reporting") to the
spawner's correlation, settle the edge, obs-record the reap. Then prove the
matrix on a scratch root, per harness (claude, codex, opencode):
- driven worker (daemon dispatch): exits nonzero → failure-mail (exists
  today — regression);
- driven worker: harness child SIGKILL'd mid-run → failure-mail (exists via
  `status.success()` — regression);
- detached worker (`elanus code spawn`): exits nonzero → structured
  failure-mail (M1);
- detached worker: **wrapper SIGKILL'd mid-run** → reaper failure-mail within
  a bounded number of ticks (the new path);
- daemon killed mid-drive and restarted → the driven delivery recovers
  (boot re-pend + `reconcile_lost_routes` — verify, wonky bit 5).

**Acceptance:** an integration test per row (harness-parameterized where the
harness binary isn't needed — the detached-wrapper kill can be tested with a
stub adapter; at least one live end-to-end run per harness logged in the
Log); the spawner receives exactly ONE completion per worker in every row
(no double-mail when a slow worker and the reaper race — the settle must be
idempotent, keyed on the edge). `cargo test` green.

### M3 — The honest capability table
A "Death and wake, per harness" section in
[../coding-harness-onboarding.md](../coding-harness-onboarding.md) (beside
the capture ladder `:156-166`, extending the resume note `:152-154`): a
(harness × mode) table with columns — failure-mail on child death,
failure-mail on wrapper death (the M2 reaper), parent wake-on-delivery
(headless: yes, via the uniform daemon resume; TUI: **no wake — inbox-pull**,
with the per-turn injection surfacing waiting mail), and mid-run injection
vector (cross-referencing the in-code matrix `codeagent.rs:2533-2537` rather
than duplicating its contents). Every cell states what M2 verified; the
out-of-scope boundary (wonky bit 4) gets its own plainly-worded paragraph:
why universal any-time wake is not buildable on event loops we don't own.

**Acceptance:** the table exists, every cell traces to an M2 run or an
existing test (no aspirational cells); the pluggable-harness checklist
(`coding-harness-onboarding.md:168-180`) gains a "report death honestly"
line so future adapters inherit the contract.

## Read these first
- The driven-path contract this extends: `src/dispatcher.rs` —
  `settle_code_deliveries` `:962` (failure classification `:965-971`),
  `route_completion` `:1049` (payload `:1136`, correlation `:1149`, FAILED
  `:1122`), `code_worker` `:1517` (`resume_capture` call `:1524`, CodeDone
  `:1540`), synthetic failures `:1475-1508`, `drive_code_deliveries` `:1272`
  (requester resolution `:1403-1404`), `reconcile_lost_routes` `:1193`,
  `tick()` order `:256-261`.
- The uniform resume/wake primitive: `src/codeagent.rs` — `resume_capture`
  `:6366`, per-harness resume commands `:6222/:6234/:6245`, stream capture
  `:6272-6274`, the single wait `:6480` + outcome `:6491-6493`, the timeout
  wrapper `:6316-6325`.
- The detached path with the hole: `codeagent.rs` — `spawn` `:952`
  (detach `:1015`), the worker's own wait `:3196-3198`,
  `emit_completion_delivery` `:3228` call / `:1094` def (payload `:1116`),
  `completion_delivery_prompt` `:1031` (status lines `:1040-1044`).
- The liveness substrate the reaper rides: `codesession::bump_last_active`
  (hook path `codeagent.rs:6766-6770`), `reap_orphans`/`reap_dead_members`
  (`codeagent.rs:3269-3276`), `inbox_for_session` `src/codesession.rs:474`
  (the TUI inbox-pull), the generic dispatch reaper pattern
  `dispatcher.rs:755-778` (`code().unwrap_or(-1)` → failed — the shape to
  mirror).
- The doc the table lands in: [../coding-harness-onboarding.md](../coding-harness-onboarding.md)
  (`:111-124` capture, `:152-154` resume, `:156-166` decision tree,
  `:168-180` checklist); the injection matrix `codeagent.rs:2533-2537`;
  [harness-modes.md](harness-modes.md) (the mode axis the table is keyed on).
- The failure-mail history: [coding-agent-dispatch.md](coding-agent-dispatch.md),
  [agent-comms-package.md](agent-comms-package.md) (failure-mail as comms).

## Log
- 2026-07-02 — Created from Tim's `_questions.md` sprint-3 pull, scoped
  honestly per grounding: the daemon-driven path is ALREADY harness-uniform
  for both death (one `child.wait()`, `status.success()` false on
  SIGKILL/nonzero) and parent wake (one `resume_capture` with real resume
  commands for all three) — refuting the "headless claude only" assumption —
  so the work concentrates on the detached `elanus code spawn` path: no
  structured `failed` field, and a SIGKILL'd wrapper mails nobody and is
  reaped by nothing. Judgment calls for Fable: a ledger spawn-edge +
  daemon reaper sweep over a supervisor/wrapper process (1); additive
  structured fields on the detached completion payload (2); wake work is
  verify-then-document, not build (3); universal any-time wake declared
  out of scope with reasons (4); the kill matrix distinguishes
  harness-child death from wrapper death (5).
