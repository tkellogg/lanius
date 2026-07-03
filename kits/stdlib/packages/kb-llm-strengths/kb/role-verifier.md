# Role: verifier (phase 4)

Verification is a **stronger** tier than implementation: pedantry is an asset
when the job is to find what the implementer missed.

## Who verifies

- **Opus on high** — see [opus.md](opus.md) (lines 10–13).
- **GPT-5.5 on high/xhigh** — its pedantry is exactly what you want here. See
  [gpt-5.5.md](gpt-5.5.md) (lines 10–13).
- **Fable for the hardest / most critical verifications**, when available — see
  [fable.md](fable.md) (lines 5–10). Fable is expensive; reserve it for the
  work where a missed defect is most costly.

## Notes

- Verify with a **stronger** model than the one that implemented
  ([role-implementer.md](role-implementer.md)).
- Impl and verify models are interchangeable for *checking* — you may swap the
  verifier mid-task. The one rule that does not flex: **planning stays with
  Claude or Fable** ([role-planner.md](role-planner.md)).
- Do not verify with GLM-5.2 by default; its home is implementation
  ([glm-5.2.md](glm-5.2.md)).

> Preferences dominate benchmarks. If Tim's experience says a model is a weak
> verifier despite a strong benchmark, that experience wins — flag the conflict.
