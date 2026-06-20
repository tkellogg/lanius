# journeys

Audience and journey notes for product decisions. Read these when changing
setup, onboarding, product language, risk/cost surfaces, or coding-agent
integration. Skip them for kernel-only refactors unless the change leaks into
the UI.

## Start here

- Need the audience lens: read [characters.md](characters.md) first.
- Changing first-run setup or "create agent": read [01-setup.md](01-setup.md).
- Adding or explaining coding agents (Codex or Claude Code): read
  [02-claude-code.md](02-claude-code.md), then the work plan at
  [../handoffs/coding-agents.md](../handoffs/coding-agents.md).
- Showing model, usage, budgets, limits, or pricing uncertainty: read
  [03-cost-visibility.md](03-cost-visibility.md).
- Showing installed/running/approved capability risk: read
  [04-risk-and-trust.md](04-risk-and-trust.md).
- Codex/Claude Code sandbox, hook, and launch adapter details: see Appendices A
  and B of [../handoffs/coding-agents.md](../handoffs/coding-agents.md).
- Changing the configure tab, instance/package settings, autonomy, or the
  setup-to-config landing: read [06-configuration.md](06-configuration.md).

## Contents

- [characters.md](characters.md) - Tim, Lily, Daniel, and Ganesh; the audience
  vocabulary and motivation map.
- [01-setup.md](01-setup.md) - installation expectations and first-agent /
  Claude Code setup pressure.
- [02-claude-code.md](02-claude-code.md) - the unified coding-agents journey
  (Codex and Claude Code): the operating envelope and the planner/worker
  orchestration. Tim and Daniel primary. Work plan in
  [../handoffs/coding-agents.md](../handoffs/coding-agents.md).
- [03-cost-visibility.md](03-cost-visibility.md) - honest cost and limit
  language: hard cap, soft limit, estimate, unknown, provider unavailable.
- [04-risk-and-trust.md](04-risk-and-trust.md) - cheap first-pass risk surfaces
  for installed, approved, running, local HTTP, fs write, and data-location
  questions.
- [06-configuration.md](06-configuration.md) - why each character opens
  configuration, what they expect, and where instance config and agent config
  blur together (shared-vs-per-agent scope, altitude, the off switch).
- [07-chatting.md](07-chatting.md) - how users primarily want to use elanus and
  interact with agents: never-ending chat, threaded conversations as the unit of
  the UI, ambient interaction, and dashboards. Work plan for the chat surface in
  [../handoffs/chat-conversations.md](../handoffs/chat-conversations.md).

## Implementation Anchors

- Setup, cost visibility, trust footprint, and capability cards:
  [ui/web/src/App.tsx](../../ui/web/src/App.tsx), especially `SetupView`,
  `costSummary`, `riskBadges`, `PackageCard`, and `ConfiguredPackageCard`.
- Browser regression coverage for these surfaces:
  [ui/web/test/ui.spec.mjs](../../ui/web/test/ui.spec.mjs).
- Product-language and setup-flow findings:
  [../ui-flows/app-search-findings.md](../ui-flows/app-search-findings.md) and
  [../ui-flows/configuration.md](../ui-flows/configuration.md).

## Skip Unless

- Do not treat persona text as an implementation contract. Use it to choose
  language, priority, and what a person needs to observe.
- For coding-agent *implementation* detail (sandbox flags, hook payloads, launch
  shape), read [../handoffs/coding-agents.md](../handoffs/coding-agents.md), not
  the journey — the journey is the user experience, the handoff is the work plan.
