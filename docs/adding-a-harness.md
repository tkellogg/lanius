---
status: deprecated
author: coding-agent-tails HM5
last-updated: 2026-07-07
---

# Adding a coding harness (deprecated — see coding-harness-onboarding.md)

This document described adding a coding harness by implementing a `trait
Harness` over zero-sized structs (`ClaudeCode`, `Codex`, `OpenCode`) held in a
static `HARNESSES` registry. **That architecture was deleted** by PH4 (`PH4
Step C: dispatch via packages + DELETE the Harness trait`, `3720df3`,
2026-06-29): harnesses are now `[[harness]]`-declaring packages, dispatched
by the kernel rather than compiled in, and the trait/registry no longer exist
in `src/codeagent.rs`.

For the current recipe, see
[coding-harness-onboarding.md](coding-harness-onboarding.md).

This file is kept only as a redirect stub so old links still resolve; do not
follow its old body as instructions.
