---
name: kb-llm-strengths
description: The model-tiering knowledge base — which model to use for planning, implementation, and verification. Read a kb/role-*.md file when you are about to dispatch or pick a model for a task.
---

# LLM strengths — the model-tiering knowledge base

This package is a knowledge base (a `kb/` subfolder, declared by the `[kb]`
marker in `elanus.toml`). It holds the model-tiering rules the harness uses when
it dispatches work: which model plans, which implements, which verifies, and the
one rule that never flexes.

It is deliberately **mutable and preference-dominated**: benchmark claims are the
floor, Tim's lived experience overrides them. When they conflict, human
preference wins and the conflict is flagged, not silently resolved.

## How this KB is laid out

- **One file per role** — start here when you are dispatching:
  - [kb/role-planner.md](kb/role-planner.md) — who plans (and who must never)
  - [kb/role-implementer.md](kb/role-implementer.md) — who implements
  - [kb/role-verifier.md](kb/role-verifier.md) — who verifies
- **One file per model** — the per-model notes the role files link into:
  - [kb/claude.md](kb/claude.md), [kb/fable.md](kb/fable.md),
    [kb/opus.md](kb/opus.md), [kb/gpt-5.5.md](kb/gpt-5.5.md),
    [kb/glm-5.2.md](kb/glm-5.2.md)

## Using it

- **Read** a `kb/role-*.md` before you pick a model for a task, or grep the
  tree: `grep -ri "who verifies" kb/`.
- **List** every enabled KB with `elanus kb list`.
- **Write** an update with `elanus kb write kb-llm-strengths kb/<file>.md` (it
  writes the file and commits it — provenance is the git log). Update the paired
  copy too: the canonical model-tiering text is mirrored in
  `.claude/skills/handoff-workflow/SKILL.md`, which configures Claude Code (and
  cannot read this KB). Keep the two from forking.
