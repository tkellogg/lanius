# ui-flows

Executable web-flow catalogs and QA findings. Read this folder when changing
`ui/web`, when a browser gesture needs durable proof, or when updating web QA.

## Start here

- Changing configure, add-ons, agent requests, or history/session flows: read
  [configuration.md](configuration.md). It is the catalog of record for
  selectors, gestures, and durable observables.
- Debugging or extending Playwright coverage: read
  [configuration.md](configuration.md), then [findings.md](findings.md), then the
  `web-qa` skill at [../../.claude/skills/web-qa/SKILL.md](../../.claude/skills/web-qa/SKILL.md).
- Working on first-run/product-language polish: read
  [app-search-findings.md](app-search-findings.md), then
  [../layering.md](../layering.md) and [../journeys/README.md](../journeys/README.md).
- Checking whether a bug is already known: read [findings.md](findings.md) for
  web QA runs and [app-search-findings.md](app-search-findings.md) for product
  search/front-door issues.

## Contents

- [configuration.md](configuration.md) - canonical flow catalog for create,
  configure, add-ons, agent requests, and history. Each flow names selectors,
  gestures, and the durable observable an assertion should trust.
- [findings.md](findings.md) - web QA run findings, fixed regressions, UX gaps,
  and regression assertions worth keeping.
- [app-search-findings.md](app-search-findings.md) - product-language and
  front-door findings through the "working with Tim" lens. Some older source
  line references may predate the React UI, but the product rule still applies.

## Implementation Anchors

- Main React dashboard: [ui/web/src/App.tsx](../../ui/web/src/App.tsx).
- Browser API relay and admin/history endpoints:
  [src/web.rs](../../src/web.rs).
- Permanent browser suite: [ui/web/test/ui.spec.mjs](../../ui/web/test/ui.spec.mjs).
- Smoke and walkthrough helpers:
  [ui/web/test/smoke.mjs](../../ui/web/test/smoke.mjs) and
  [ui/web/test/walkthrough.mjs](../../ui/web/test/walkthrough.mjs).

## Durable Assertion Rule

Prefer state that survives a re-render or reload: reloaded field values, nav rows,
visible degraded/healed states, `#setup-status`, and backend log lines. Treat
notes such as `#cfg-note`, `#na-note`, and button labels as liveness only unless
the catalog says otherwise.
