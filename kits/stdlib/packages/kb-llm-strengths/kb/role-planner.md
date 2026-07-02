# Role: planner (phases 1–2)

Planning is the highest-leverage phase — intent capture and spec work — and the
one place model choice does **not** flex.

## Who plans

- **Claude**, ideally at high/xhigh effort — see [claude.md](claude.md) (the
  "planning" section, lines 5–9).
- **Fable**, when available — see [fable.md](fable.md) (lines 5–10). Fable is
  unparalleled at planning but expensive; bank it for planning (and the hardest
  verifications).

## The rule that never flexes

**Only Claude or Fable plan. Never hand planning to GPT-5.5 or GLM-5.2.** This
holds even when impl/verify models are being swapped mid-task for credit reasons
(that swap is allowed for building and checking — see
[role-implementer.md](role-implementer.md) and
[role-verifier.md](role-verifier.md) — but planning stays put).

Why: Claude "gets Tim" better than the others, so it is trusted for intent and
spec work; Fable is the strongest planner outright. GPT/GLM are strong builders
and checkers ([gpt-5.5.md](gpt-5.5.md), [glm-5.2.md](glm-5.2.md)) but are not
trusted to own the plan.

> Preferences dominate benchmarks: if a benchmark says a cheaper model plans
> "well enough," Tim's lived experience here overrides it. Flag the conflict;
> do not silently promote GPT/GLM into planning.
