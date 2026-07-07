---
name: handoff-workflow
description: >-
  The journey → handoff → implement → verify pipeline for shipping substantial
  work on lanius, with deliberate model-tiering across the phases. Use when Tim
  asks to plan a feature, write or "knock out" handoff docs, implement a spec,
  or orchestrate implementation + verification — anything bigger than a quick
  edit. Encodes who plans vs implements vs verifies, which model/effort fits
  each phase, how to dispatch clean-context worker agents, and the containment +
  commit discipline that keeps the repo clean. Fable should *ALWAYS* follow the
  second-level workflow specified below.
---

# Handoff workflow

How we ship non-trivial work on lanius. Four phases, two context boundaries, and
a deliberate choice of model at each phase. The whole point is that **the model
that plans is not the model that implements, and the model that implements is not
the model that verifies** — each phase plays to a different model's strength, and
clean context keeps the implementer and verifier honest.

## The four phases

| # | Phase | Artifact | Context | Model strength wanted |
|---|---|---|---|---|
| 1 | **Journey** (if needed) | aspirational doc — Tim's intent | **same as 2** | planning / "gets Tim" |
| 2 | **Handoff** | a spec, milestone'd | **same as 1** | planning + codebase analysis |
| 3 | **Implementation** | code | **fresh, clean** | implementation grind |
| 4 | **Verification** | verdict + fixes | **fresh, clean, separate from 3** | adversarial rigor |

Phases **1 & 2 share one agent/context** (the planner reasons about intent and
the spec together). Phases **3 & 4 are separate agents, each with clean context**
— the implementer must not be biased by the planner's chain-of-thought, and the
verifier must not be biased by the implementer's.

### 1. Journey (optional) — capture the intent
A highly **aspirational** doc in `docs/journeys/` covering what's in Tim's head:
the felt problem, first-person, the outcome that would delight him — *not* a plan.
Its job is to be the thing a later agent reasons against to decide whether an
approach or implementation actually does what Tim wanted. Write one when the
intent is fuzzy, contested, or easy to satisfy on paper while missing the point.
Skip it when the handoff's intent is already obvious.

### 2. Handoff — the spec
A planner writes `docs/handoffs/<name>.md`: a precise accounting of the work,
**grounded in the actual codebase** (read the code; cite real files, functions,
line anchors). Shape that has worked:
- frontmatter: `status` (planned | in-progress | verifying | done), `author`,
  `last-updated`.
- the **"wonky bits" / decisions to confirm** up front — the non-obvious calls
  that change the shape of the work.
- **milestones** (M1, M2, …), each with a concrete **Acceptance** clause.
- a **"Read these first"** list (the journey, related handoffs, the key source
  files) and a **Log** of what was learned/decided and when.
- be honest about residuals and gating: name what's deferred and *why*.

This planner step is where Claude (or Fable) earns its keep — see Models.

### 3. Implementation — fully build it
A **fresh agent with clean context** implements the handoff. The instruction is
to **fully implement all milestones** in scope — not a sketch. If you deliberately
scope to a subset (the cleanly-shippable core), say so explicitly and **leave a
code comment noting each deferred milestone and why**, then capture the remainder
in a follow-up handoff. A weaker/cheaper model is the right tool here; the spec
carries the thinking.

### 4. Verification — adversarial, separate agent
A **different fresh agent**, a **stronger** model, verifies against the handoff's
acceptance clauses. It is adversarial: assume the implementer guessed wrong on the
fiddly bits (a schema key, a flag, an edge case) and try to prove it. It must
actually **build and run the tests**, and ideally check assumptions against
reality (the real binary, the real consumer of a wire shape). Return a
**structured verdict** — `{ pass, build_ok, tests_ok, issues:[{severity,
location, problem, fix}], summary }` — and run a **bounded fix loop**: verifier
finds issues → implementer fixes → re-verify, ~3 rounds max, then surface
whatever remains.

## Models — choose per phase

> Canonical copy: the `kb-llm-strengths` KB (`kb/role-*.md`, `kb/*.md`, in
> `kits/stdlib/packages/kb-llm-strengths/`). This section configures Claude Code,
> which cannot read that KB, so it keeps its own copy — update both when the
> tiering changes.

Model choice is a big deal; it is the main lever this skill exists to pull.

- **Planning (phases 1–2): Claude, ideally high/xhigh effort — or Fable.** Claude
  "gets Tim" better than the others, so trust it for intent and spec work.
  **Fable is unparalleled at planning** but expensive: bank it for planning, and
  occasionally for verifying very hard or very critical work, when it's available.
  **Only trust Claude (or Fable) for planning.** Do not hand planning to GPT/GLM.
- **Implementation (phase 3): a weaker/cheaper tier.** Opus on **medium**,
  GPT-5.5 on **medium**, or GLM-5.2 on **medium/high**. GPT-5.5 is extremely smart
  and pedantic — a strong implementer.
- **Verification (phase 4): a stronger tier.** Opus on **high**, GPT-5.5 on
  **high/xhigh**. GPT-5.5's pedantry is an asset here. Fable for the hardest/most
  critical verifications when available.
- **Interchangeable, with one exception.** Impl and verify models can be swapped
  mid-task — e.g. start implementation on GPT-5.5 and finish on Opus if GPT credits
  run out. They're roughly fungible for *building* and *checking*. The one rule
  that does **not** flex: **planning stays with Claude or Fable.**
- **GLM-5.2: placement still uncertain** — fits somewhere in impl (medium/high);
  update this skill as its strengths become clear.

## Dispatching the worker agents

Two ways to get clean-context impl/verify agents; pick by whether you need a
non-Claude model.

- **Staying on Claude/Opus → the `Workflow` tool.** `agent()` with
  `{ model: 'opus', effort: 'medium' }` for impl and `{ effort: 'xhigh' }` for
  verify gives clean separate contexts, a structured-output schema for the
  verdict, and a natural place to code the fix loop. This is the default when the
  task fits Opus.
- **Cross-model → `lanius code`.** Dispatch the underlying harness:
  `lanius code codex "<task>"` (GPT-5.5), `lanius code opencode "<task>"`
  (GLM-5.2, via the configured provider), `lanius code claude --worker "<task>"`
  (Opus). Use `spawn`/`deliver` for async. Each worker is its own clean context.

The orchestrator (you, as Claude) acts as **planner + conductor**: do phases 1–2
yourself, then dispatch 3 and 4 to workers and drive the fix loop. Review the diff
and **commit yourself** — see discipline below.

## Containment & commit discipline (hard-won)

Worker agents have wrecked the repo before (junk branches, commits through a /tmp
clone whose `origin` pointed home, stray servers). Bake these into every worker
prompt:

- **No git, ever, in workers:** no commit/branch/checkout/push/stash/add. Leave
  changes unstaged. **The orchestrator commits.**
- **Scope the filesystem:** edit only the paths the task needs (e.g. `src/`).
  Don't touch unrelated trees — especially files with uncommitted in-flight edits.
- **Probes run in `/tmp`,** never against the live repo; kill any server/daemon a
  worker starts.
- **Commit as the orchestrator, one scoped commit per handoff.** Stage only the
  files your work produced; never sweep in unrelated dirty files. End commit
  messages with the `Co-Authored-By` trailer.
- **Scope to a cleanly-committable unit.** Prefer landing a coherent subset
  (with deferrals noted) over a sprawling change that entangles other in-flight
  work. When a change feeds an existing consumer (a wire shape, an API), keep it
  **backward-compatible** so you don't have to touch — or break — that consumer.

## The loop, end to end

Plan (you) → write/confirm the handoff → dispatch impl (clean worker, weaker
model) → dispatch verify (clean worker, stronger model) → fix loop until the
verdict passes → you review the diff, build/test once more, and commit → update
the handoff `status` and, if milestones remain, write the follow-up handoff.


## Second-level Handoff Workflow
Fable (specifically) should *always* use the second-level workflow to conserve
tokens and direct Fable's biggest strengths at the best fit problems.
second-level-workflow.md
