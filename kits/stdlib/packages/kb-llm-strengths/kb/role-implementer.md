---
title: Role: implementer (phase 3)
description: Who implements — the weaker/cheaper tier that builds the plan faithfully.
tags: [roles, implementation]
---
# Role: implementer (phase 3)

Implementation is a weaker/cheaper tier than planning: the plan is already
written, so the job is to build it faithfully.

## Who implements

- **Opus on medium** — see [opus.md](opus.md) (lines 5–9).
- **GPT-5.5 on medium** — extremely smart and pedantic, a strong implementer.
  See [gpt-5.5.md](gpt-5.5.md) (lines 5–9).
- **GLM-5.2 on medium/high** — placement still firming up; fits here. See
  [glm-5.2.md](glm-5.2.md) (lines 5–9).

## Notes

- Impl and verify models are **interchangeable, with one exception**: you may
  swap the implementer mid-task (e.g. start on GPT-5.5, finish on Opus if GPT
  credits run out). They are roughly fungible for *building*. The exception is
  that **planning never flexes** ([role-planner.md](role-planner.md)).
- Do **not** use Claude/Fable here by default — bank them for planning
  ([claude.md](claude.md), [fable.md](fable.md)); they are the expensive tier.
- After implementing, hand to a **stronger** tier to verify
  ([role-verifier.md](role-verifier.md)).
