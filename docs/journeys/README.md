# journeys

Audience and journey notes for product decisions. Read these when changing
setup, onboarding, product language, risk/cost surfaces, or coding-agent
integration. Skip them for kernel-only refactors unless the change leaks into
the UI.

## Start here

- Need the audience lens: read [characters.md](characters.md) first.
- Changing first-run setup or "create agent": read [01-setup.md](01-setup.md).
- Adding or explaining Claude Code: read [02-claude-code.md](02-claude-code.md).
- Showing model, usage, budgets, limits, or pricing uncertainty: read
  [03-cost-visibility.md](03-cost-visibility.md).
- Showing installed/running/approved capability risk: read
  [04-risk-and-trust.md](04-risk-and-trust.md).
- Designing future Codex support: read [05-codex-integration.md](05-codex-integration.md).

## Contents

- [characters.md](characters.md) - Tim, Lily, Daniel, and Ganesh; the audience
  vocabulary and motivation map.
- [01-setup.md](01-setup.md) - installation expectations and first-agent /
  Claude Code setup pressure.
- [02-claude-code.md](02-claude-code.md) - current open questions and acceptance
  criteria for a Claude Code setup path. It deliberately does not pretend the
  integration is specified.
- [03-cost-visibility.md](03-cost-visibility.md) - honest cost and limit
  language: hard cap, soft limit, estimate, unknown, provider unavailable.
- [04-risk-and-trust.md](04-risk-and-trust.md) - cheap first-pass risk surfaces
  for installed, approved, running, local HTTP, fs write, and data-location
  questions.
- [05-codex-integration.md](05-codex-integration.md) - later design sketch for
  launching the real Codex TUI inside the elanus operating envelope.

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

- Do not read [05-codex-integration.md](05-codex-integration.md) for ordinary
  setup UI work; it is future integration research.
- Do not treat persona text as an implementation contract. Use it to choose
  language, priority, and what a person needs to observe.
