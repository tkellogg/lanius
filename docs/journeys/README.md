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
- [08-dispatching-a-worker.md](08-dispatching-a-worker.md) - the coding-agent's
  OWN seat: a first-person account of a session under elanus trying to "dispatch
  a codex worker" and mostly failing (no front door, undocumented launch verb,
  silently dropped prompt, sync-vs-async confusion). Work plan in
  [../handoffs/coding-agent-dispatch.md](../handoffs/coding-agent-dispatch.md).
- [09-colliding-with-a-sibling-agent.md](09-colliding-with-a-sibling-agent.md) - a
  coding agent discovering, late at commit time, that another session was working
  the same repo, and the three ascending ways elanus could have made the sibling
  ambient instead of a surprise. Work plan in
  [../handoffs/sibling-awareness.md](../handoffs/sibling-awareness.md).
- [10-what-did-the-agent-read.md](10-what-did-the-agent-read.md) - the provenance
  story: an agent prompt-injected through a file it read, and the question its human
  then couldn't answer — "what did you read?" — because reads, unlike writes and
  tool calls, leave no trace. The why behind the read camera
  ([../sandbox.md](../sandbox.md) "The read camera").
- [11-profiles.md](11-profiles.md) - profiles as bundles of capabilities you bolt
  onto an agent, and the three capabilities the journey actually wants on top of the
  profile machinery that already ships: **memory blocks** (named, editable,
  evolving prompt chunks — the keystone), **inter-agent comms** (computed blocks +
  priority injection over the existing mailbox/rooms), and **work estimation**
  (estimate-after-plan → retro → adjust a block). Work plans decomposed into
  [../handoffs/memory-blocks.md](../handoffs/memory-blocks.md),
  [../handoffs/agent-comms-package.md](../handoffs/agent-comms-package.md), and
  [../handoffs/work-estimation.md](../handoffs/work-estimation.md).
- [16-the-helper.md](16-the-helper.md) - the built-in helper as a **UI
  concierge**: "Ask" buttons on confusing elements, view-context injection, and
  the 2026-07-08 first-encounter failures (dead air, surprise agent-spawn from
  merely opening a tab, the vanished message). Read before touching the helper
  or any "explain this UI" affordance.
- [ui-preferences.md](ui-preferences.md) - cross-cutting UI expectations:
  options over text boxes, agent-operated UI, navigation (URLs change with
  pages, words over icons, no kernel vocabulary onscreen), log-like surfaces
  (collapsed rows, click-to-expand JSON, never the default page), and color
  (text outranks chrome in contrast).
- [reaching-the-user.md](reaching-the-user.md) - **stub**: the human-proxy / EA
  actor. A person is one identity reachable many ways (elanus, phone, Bluesky,
  Teams, email) with varying effectiveness; the idea is a rules-engine-or-LLM
  actor that picks channels, escalates across them, and brokers private comms —
  the *policy layer* on the already-built phonebook/recall/egress/human-proxy
  rails ([../identity.md](../identity.md), [../actors.md](../actors.md)). Split
  out of [../handoffs/chat-rendering.md](../handoffs/chat-rendering.md).

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
