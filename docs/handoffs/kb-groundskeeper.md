---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: kb-groundskeeper — the script checks, then the diff pipeline

Decomposed from [knowledge-base.md](knowledge-base.md) build steps **B5 + B6**
(ruling D4). Two rungs on the variety ladder: a **script rung first** (a cron
package that validates pointers, orphans, and staleness — no LLM, owner report),
then the **diff pipeline** — a cheap ambient **compactor** that emits unified
diffs and an expensive **ratifier** that applies or bounces them *with feedback*.
This is deliberately elanus's **first auto-approve pipeline**: the ratifier
stands in for the human at a quality tier the human trusts. It is **setup-gated
— nothing runs before setup**. Depends on [kb-core.md](kb-core.md) (write path +
pointer blocks) and [kb-search.md](kb-search.md) (the compactor sweeps the
corpus). This is the highest-risk KB handoff; scope M1 cleanly and gate the rest.

## Wonky bits / decisions to confirm (my judgment calls flagged)

1. **Setup surface: use the config model, not a bespoke setup CLI.** D4 requires
   explicit human configuration — which two models, cadence, token budgets —
   "in the spirit of the work-estimation package's opt-in." I checked how
   work-estimation actually opts in: there is **no** profile toggle and **no**
   setup subcommand; estimation rides existing substrates (a memory block + an
   `obs/estimate/<session>` boundary), and only its *cron backstop* is gated by
   `elanus approve estimation` (`kits/core/packages/estimation/`). That opt-in is
   **lighter than what this pipeline needs** — the pipeline must persist concrete
   model choices, a cadence, and budgets. **My call: use the config model
   (docs/config.md).** The package declares `[config]` keys (compactor model,
   ratifier model, cadence, per-pass token budget) and the human sets them with
   `elanus config set kb-groundskeeper.<key>` (`src/configcli.rs`, committed on
   the live branch, `src/main.rs:1204`). The compactor **cron is additionally
   gated by `elanus approve`** (like estimation's backstop). "Nothing runs before
   setup" = the cron is inert until the model keys are set **and** the package is
   approved. *Fable: confirm config-model + approve over a bespoke `elanus kb
   groundskeeper setup` verb.*

2. **"Read the two models from the llm-strengths KB itself" (D4/D5) is an
   *informing* read, not a per-run derivation.** D4 says the two models are "both
   read from the LLM-strengths KB." Deriving them by searching the KB on every
   pass costs tokens each run. **My call: the setup step consults the KB (via
   `search_knowledge`) to inform the human's choice — cheap = compactor,
   expensive = ratifier, planning-never-flexes stays out of it — then persists a
   concrete choice in config.** The KB is the source of the *recommendation*; the
   config is the *committed decision*. Flag this as a small departure from a
   literal reading of D4.

3. **First auto-approve pipeline — the ratifier is the trust boundary.** The
   ratifier applies diffs via the kb-core write path (sandbox-gated write + git
   commit) with **no human in the loop**, which is new for elanus. The gate that
   makes this safe is the setup: the human chose a model tier they trust and
   turned it on deliberately. The compactor's proposals are inert (a list of
   diffs) until the ratifier acts. Keep every applied diff a git commit (kb-core
   D2) so the human can audit and revert.

4. **Compactor memory, not a compactor KB (D4).** The compactor keeps its own
   **memory blocks** — what it tried, what the ratifier bounced, the feedback —
   *not* a KB of its own. This is ordinary `context_store` block writing, which
   hits the concurrency dup-row hazard
   ([notes-scaling-and-storage.md](../notes-scaling-and-storage.md) §2) if the
   compactor writes blocks concurrently; the pipeline is single-threaded per pass
   so this is low-risk, but note the [storage-hardening.md](storage-hardening.md)
   dependency for safety.

5. **Launch + feedback mechanics.** Stages launch agents via `spawn_core`
   (`src/agentcli.rs:162`): it emits to the target profile's mailbox with an
   `agent-spawn-<uuid>` correlation and returns a descriptor; the reply/failure
   comes back as mail on that correlation (async by construction). The
   compactor→ratifier bounce is mail on a correlation; ratifier feedback lands in
   the compactor's memory blocks. The cron→exec self-kick pattern is
   `watchdog`/`escalation`/`estimation` (`[[cron]]` emits a topic the package
   also `subscribe`s).

## Milestones

### M1 — the script groundskeeper (B5, rung 1): no LLM
A cron package (mirror `kits/core/packages/estimation/`,
`packages/watchdog/`, `packages/escalation/` cron→exec self-kick). On each
sweep it: (a) validates every pointer block's `meta.{path,lines,sha}` (kb-core
M3) — the file exists, the lines exist, the sha matches; (b) finds orphan `kb/`
files (no pointer referencing them — informational); (c) flags staleness (file
changed since the recorded sha). It mails the owner a report (like estimation's
report). **Zero LLM calls.**

**Acceptance:** on a seeded corpus with one broken-path pointer, one stale-sha
pointer, and one orphan file, each breakage class appears in the owner report;
`grep`/instrumentation confirms **zero** LLM calls on the sweep; `cargo test`
green for the pure checker logic.

### M2 — the setup flow (B6, gate): config + approve, informed by the KB
Declare `[config]` keys on the package (compactor model, ratifier model, cadence,
per-pass token budget) and wire `elanus config set kb-groundskeeper.<key>`. The
setup guidance (a skill) has the human consult the llm-strengths KB (via
`search_knowledge`) to pick cheap vs expensive models (wonky bit 2). The
compactor cron is gated by `elanus approve kb-groundskeeper`. Until **both** the
model keys are set **and** the package is approved, the pipeline is inert.

**Acceptance:** with no config and no approval, no compactor/ratifier ever
launches (the cron is a no-op); after `elanus config set` of the two models +
cadence + budget **and** `elanus approve`, the cron becomes live; the setup skill
points at the KB for the model choice; a unit test asserts the "inert before
setup" gate.

### M3 — the diff pipeline (B6, rung 2): compactor proposes, ratifier ratifies
The cron (once set up) spawns the **compactor** (`spawn_core`, configured cheap
model) to sweep the corpus and emit **unified diffs** (consolidations, link
fixes, conflict annotations) — a list; **nothing applied**. For each diff, spawn
the **ratifier** (`spawn_core`, configured expensive model) which either
**applies** it via the kb-core write path (write + git commit) or **bounces** it
*with feedback*. Bounced feedback lands in the compactor's memory blocks so the
next pass learns. Measure and log cost (tokens/dollars) per pass — reuse the
`estimate` substrate if convenient.

**Acceptance:** a seeded contradiction between two kb entries yields a compactor
diff; a deliberately bad diff is **bounced with feedback** that appears in the
compactor's memory blocks; nothing is applied before ratification (a git-log
check shows only ratifier-authored commits); cost per pass is measured and
logged; the whole exchange is reconstructable from the obs trail.

## Read these first
- The settled design: [knowledge-base.md](knowledge-base.md) D4 (the two-stage
  diff pipeline, compactor memory, setup-gated), D5 (models read from the KB),
  build steps B5/B6.
- The dependencies: [kb-core.md](kb-core.md) (write path M4, pointer blocks M3),
  [kb-search.md](kb-search.md) (the sweep surface),
  [storage-hardening.md](storage-hardening.md) (concurrent block writes),
  [../notes-scaling-and-storage.md](../notes-scaling-and-storage.md) §2.
- The cron→exec self-kick pattern: `kits/core/packages/estimation/elanus.toml`
  (`[[cron]]` `:37` + matching `subscribe`), `packages/watchdog/elanus.toml:13`,
  `packages/escalation/elanus.toml:14` (6-field seconds cron + `payload`).
- The opt-in exemplar (and why it is too light here): the `estimation` package +
  `src/estimatecli.rs` / `src/estimate.rs` (package-local `pricing.toml`, cron
  backstop gated by `elanus approve`, no profile toggle).
- The launch + resume rails: `spawn_core` (`src/agentcli.rs:162`, `SpawnRequest`
  `:40-50`); the `launch_agent` tool caller (`src/exec.rs:2208-2255`); mail
  comes back on the returned correlation. The config surface: `src/configcli.rs`,
  `src/manifest.rs:87-92` (`[config]` `agent_tunable`).

## Log
- 2026-07-07 — Confirmed shipped+merged on main (sprint-4 KB arc, merged
  `80a23c7`); status flipped to `done` (was stale at `planned`). 559 tests green.
- 2026-07-02 — Decomposed from knowledge-base.md B5/B6 by Opus (planner) under
  Fable. Grounded against the sprint-4 worktree: work-estimation's opt-in is
  lighter than this pipeline needs (no profile toggle, no setup verb; only its
  cron backstop is `elanus approve`-gated) — so the setup surface is the config
  model + approve (wonky bit 1), not a copy of estimation's shape. `spawn_core`
  (`src/agentcli.rs:162`) is the programmatic launch; replies ride the returned
  correlation (async). Cron→exec self-kick precedent verified in estimation/
  watchdog/escalation. Highest-risk KB handoff: first auto-approve pipeline,
  multi-agent feedback loop, depends on the most other pieces. Judgment calls
  flagged: config-model setup (1), KB informs but config persists the model
  choice (2), ratifier-is-the-trust-boundary (3), compactor memory not KB (4).
