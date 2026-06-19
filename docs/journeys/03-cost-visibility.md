---
name: Cost visibility
description: Journey notes for making model cost, limits, and usage understandable without fake precision.
---

# Cost Visibility

Cost is a setup concern, not just an analytics concern. Daniel and Lily both
need a reason to trust that Elanus will not quietly burn money.

## Product Promise

Elanus should help users understand and bound model/tool usage.

It does not need perfect billing data in the first pass. It does need honest
labels:

- hard cap
- soft limit
- estimate
- unknown
- provider unavailable

Unknown is acceptable. Fake precision is not.

## First-Pass UI

Show cost-adjacent state during setup and on each agent:

- selected model
- provider/base URL/API-key-env status
- max run steps for one activation
- throttle/budget limits if configured
- whether autonomy is off/manual/assisted/autonomous
- recent activity count if history is available

Explain that max run steps caps one activation's model/tool loop, not lifetime
conversation cost.

## Later UI

- per-agent usage history
- per-capability usage history
- model pricing metadata where providers make it available
- estimated cost per run
- budget alerts
- "expensive lately" signals
- exportable usage summary

## Acceptance Criteria

- Daniel can see a cost-control reason to keep using Elanus before touching raw config.
- Lily can pick a model with a clear cost/performance hint.
- Tim can inspect the actual fields that enforce the limits.
- The UI visually separates hard limits from estimates.

