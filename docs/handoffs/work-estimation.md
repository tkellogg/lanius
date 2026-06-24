---
status: done
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-23
---

> **Status:** E1–E3 shipped. The no-kernel-data-model constraint held — state
> lives in `context_blocks` + `obs/estimate` events (no estimation tables). The
> placement question is **resolved**: the package now lives in `kits/core/`
> (non-protected), so estimation is an **additive, revoke-guarded bolt-on** —
> `elanus revoke estimation` turns off the cron sweep without `--force` (it is
> not in `protected_packages`), matching the journey's "additive/optional"
> framing instead of being protected/always-on under `kits/stdlib`. Note that
> `kit add` is a human install gesture, so estimation's grant is auto-approved
> on install (the sweep runs immediately); `elanus kit add core --pending`
> stages it for `elanus approve estimation` if you want to review first. LLM
> "why it missed" reflection and the live mid-session watcher remain documented
> follow-ons.

# Work estimation

Let an agent **estimate its work right after it plans**, then count actuals
against that estimate, then **retro on the miss and adjust a memory block** so the
next estimate is better. The journey's loop:

> An agent, right after it's got the plan figured out, provides an estimate. From
> that moment onwards it all counts against that estimate. The process comes by
> later, does a retro on why it missed the mark, and adjusts memory blocks for a
> better future estimate.

Two hard constraints from Tim:

1. **No kernel data model.** *"Work estimation is by no means a built-in thing,
   it has no representation in the data model."* So this is a **package** built
   entirely from existing primitives: it stores state in **blocks** + **obs
   events**, reads actuals from the **obs stream**, and owns any package-local
   state itself. No new tables, no kernel types.
2. **Dollars are the normalizer, but estimate everything.** *"Estimates are
   always in dollars… but we should also estimate in all other things. Dollars is
   the great normalizer across models."* So an estimate is multi-dimensional
   (dollars, turns, tokens, wall-clock); dollars is the primary, cross-model axis.

**Hard dependency:** memory-blocks ([memory-blocks.md](memory-blocks.md)) — the
running estimate and the learned estimation heuristic are blocks. **The
dollars dependency is the live risk** (see wonky bits).

## What exists / what's missing

- **Actuals are observable.** Every coding session already streams to the bus
  under `obs/agent/<noun>/<session>/…` and is materialized to sqlite by
  `src/code_projection.rs` (the coding-agent-observability handoff). Turns, tool
  calls, and wall-clock are derivable from that today.
- **Tokens/dollars are NOT uniformly available.** Token usage is harness-shaped:
  opencode exposes cost/usage (`opencode stats`, session API), `codex exec --json`
  emits usage events, Claude Code carries usage in `-p --output-format json` (not
  the hook payloads). The package must read usage per harness — there is no single
  kernel usage feed.
- **There is no pricing table.** `src/models.rs` has **no** price/cost data, and
  the cost-visibility journey ([../journeys/03-cost-visibility.md](../journeys/03-cost-visibility.md))
  is explicit that dollars are *"unknown until real pricing data exists."* Dollars
  require a per-model `$/token` map that does not exist yet.

## Decisions to confirm (the wonky bits)

1. **Where do dollars come from?** Dollars are the normalizer but nothing sources
   them. Pick one: **(a)** ship a small per-model pricing map *inside this
   package* (fastest, package owns it — fits "no kernel data model"); **(b)** add
   a kernel pricing source first and depend on it (cleaner, slower, and arguably a
   cost-visibility deliverable, not this one). **Recommend (a)** — a package-local
   `pricing.toml` keyed by model id, with tokens×price → dollars, so estimation
   ships without blocking on a kernel change. Confirm.
2. **The estimate boundary = the agent declares it.** "Right after it's got the
   plan figured out" needs a start signal. Simplest: the act of recording the
   estimate *is* the boundary (the agent calls the estimate verb when its plan is
   set; everything after counts). Don't try to auto-detect plan-completion.
   Confirm vs. hooking a plan-mode-exit signal.
3. **Retro = cron step first, agent reflection later.** A scheduled/`Stop`-driven
   step that computes actual-vs-estimate variance and appends it to the block is
   the MVP. The journey's "*why* it missed" reflection — an LLM judging the
   variance and rewriting the heuristic — is a natural follow-on (an estimator
   agent), not the first cut. Confirm the split.
4. **No live watcher needed for the MVP.** The journey imagines an "MQTT listener
   watching traffic." Reading the already-materialized obs at session end
   (`code_projection`) gives the same accounting without a long-running
   subscriber. Add the live watcher only if running, mid-session burn-down is
   wanted. Confirm MVP = retro-at-end.

## Milestones

### E1 — Estimate capture (the plan-time declaration)
A skill + verb (`elanus estimate set --dollars … --turns … --tokens …
--wall-clock …`, or the MCP tool) the agent calls once its plan is set. Records
the estimate as (a) a `estimate` block on the session/run scope (so it shows in
context — the agent sees what it committed to) and (b) an `obs/estimate/<session>`
event (so it's on the bus for the retro). Marks the count-from boundary.

**Acceptance:** an agent records a multi-dim estimate; `context render` shows the
`estimate` block; an `obs/estimate/<session>` event carries the same dims and a
timestamp; calling it twice updates (latest wins).

### E2 — Actuals + variance (dollars-normalized)
Compute actuals from the obs stream from the estimate boundary onward: turns,
tool calls, wall-clock from `code_projection`; tokens/dollars from the per-harness
usage source × the pricing map (wonky bit 1). Surface a running (or end-of-run)
`estimate-vs-actual` computed block: each dimension, with dollars as the headline,
and a variance (over/under).

**Acceptance:** for a finished session with a recorded estimate, the package
reports actual dollars/turns/tokens/wall-clock and the per-dimension variance vs
the estimate; dollars are computed via the documented pricing map; a session with
no estimate is simply skipped (no crash).

### E3 — Retro → learned block (the loop closes)
On session end (`Stop`/`SessionEnd`) or a cron sweep, append the variance to a
durable `estimation` block (agent or profile scope) — the default-that-evolves
mechanism from memory-blocks. This block is what future E1 estimates read, so the
estimate improves over time. (LLM-driven "why it missed" reflection is a noted
follow-on; the MVP records the variance + a terse heuristic note.)

**Acceptance:** after a run, the `estimation` block gains a dated entry with the
miss (e.g. "estimated $0.40 / 8 turns, actual $0.62 / 13 turns; underestimated
tool-heavy work"); a subsequent E1 in a profile carrying that block has the prior
misses in context. The block survives across sessions (durable, per memory-blocks
M2).

## Read these first
- The why: [../journeys/11-profiles.md](../journeys/11-profiles.md)
  ("Estimating work" and "Additive").
- The dollars problem: [../journeys/03-cost-visibility.md](../journeys/03-cost-visibility.md)
  (hard cap vs estimate vs unknown) and `src/models.rs` (no pricing exists).
- The substrate: [memory-blocks.md](memory-blocks.md) (estimate + learned
  heuristic are blocks).
- Actuals: `src/code_projection.rs` and the
  [coding-agent-observability.md](coding-agent-observability.md) handoff (the obs
  stream this reads).

## Log
- **2026-06-23 — E1–E3 shipped** (impl on Opus medium → adversarial verify on Opus
  high, 1 round, `pass`; `cargo test` 274, 8 estimation tests). Delivered as a thin
  `elanus estimate` CLI (`src/estimate.rs`, `src/estimatecli.rs`, wired in
  `src/main.rs`) + a Stop-hook retro wire (`src/codeagent.rs`) + a package
  (`kits/core/packages/estimation/` — `elanus.toml`, `pricing.toml`, `SKILL.md`,
  `scripts/sweep` cron backstop). **No estimation kernel data model** (verified: no
  new table in `db.rs`) — state is `estimate` / `estimate-vs-actual` / `estimation`
  blocks + `obs/estimate/<session>` events.
  - **E1:** `elanus estimate set --dollars/--turns/--tokens/--wall-clock` writes the
    `estimate` block (session scope) + emits `obs/estimate/<session>` with a
    boundary timestamp; latest-wins.
  - **E2:** actuals come from the obs projection (turns/tool-calls/wall-clock) ×
    `pricing.toml` ($/token) for dollars (the headline). ABSENT token usage is
    graceful — `dollars_unavailable:true`, other dims still reported; an unpriced
    model yields no dollars rather than a fabricated one; a no-estimate session is
    skipped.
  - **E3:** the Stop hook (cron `sweep` as backstop) appends a dated miss to a
    durable agent-scope `estimation` block, once-per-session via a marker block; a
    new session reads prior misses via `load_session_blocks` — the
    future-estimate-improves loop closes.
  - **Accepted minors:** (1) the tool-calls dimension is actual-only (no estimate to
    vary against — `Estimate` has no tool_calls field); (2) a retro-timing edge (if
    Stop fires before the final obs folds, a miss could record 0s and is sticky) —
    mitigated because `report()` flushes the trace first; acceptable for the MVP.
  - **2026-06-24 — placement resolved:** moved `kits/stdlib/packages/estimation/`
    → `kits/core/packages/estimation/` (core has no `kit.toml protected=true`).
    Estimation is now **non-protected → revoke-guarded**: `elanus revoke
    estimation` turns off the cron sweep without `--force` (it is not in
    `protected_packages`), matching the "additive/optional" framing. Note the
    grant axis is separate: `kit add` (and `init --kit core`) is a human install
    gesture, so estimation's cron grant is **auto-approved** on install and the
    sweep runs immediately — `elanus approve estimation` is a no-op unless you
    installed with the explicit `--pending` flag. The only load-bearing code change
    was the `pricing.toml` fallback lookup in `src/estimatecli.rs::pricing_path`
    (`kits/stdlib/...` → `kits/core/...`); the primary lookup is already the
    profile package-path (`root.packages()/estimation/pricing.toml`), so an
    installed copy resolves dynamically and dollars still compute.
    - **Accepted minors (relocation verify):** (1) the relocated `pricing.toml`
      fallback path has no PERMANENT unit test — the changed line is behaviorally
      proven (a throwaway test confirmed dollars resolve from the `kits/core` fallback)
      but the shipped suite only exercises the primary package-path lookup; optional
      follow-on. (2) PRE-EXISTING, not this move: `STOCK_KIT_FILES` (`src/initcmd.rs`)
      embeds only escalate/harness-doctrine/self-modify into a binary `init`, NOT
      estimation — so a binary-installed root never materializes the estimation package
      or its pricing fallback; dev/repo builds resolve it from the repo `kits/` dir.
      Worth a separate task if estimation should ship to binary installs.
- **2026-06-23 — planning.** Confirmed with Tim: estimation is a **package**, no
  kernel data-model representation; dollars are the cross-model normalizer but
  estimate every dimension. Identified the live risk: **dollars have no source**
  (`models.rs` has no pricing; cost-visibility says dollars are unknown) — E2
  needs a pricing map, recommended package-local to avoid blocking on a kernel
  change. MVP simplifications: estimate boundary = agent declares; retro =
  cron/Stop step (LLM reflection later); actuals = read materialized obs at end
  (no live watcher).
