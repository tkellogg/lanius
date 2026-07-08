---
title: Opus
description: A workhorse for both building and checking; effort dials the tier.
tags: [models, implementation, verification]
---
# Opus

> A workhorse for both building and checking; effort dials the tier.

## Implementation (phase 3)

- **Opus on medium** is a solid implementer. Use it when the task fits Opus and
  you do not need a non-Claude model. See
  [role-implementer.md](role-implementer.md).

## Verification (phase 4)

- **Opus on high** is a strong verifier — the higher effort buys the pedantry a
  verifier needs. See [role-verifier.md](role-verifier.md).

## Notes

- Impl and verify are interchangeable: you may swap Opus in or out mid-task
  (e.g. finish a GPT-5.5 implementation on Opus if credits run out).
- Not a planner — planning stays with [claude.md](claude.md) / [fable.md](fable.md).
