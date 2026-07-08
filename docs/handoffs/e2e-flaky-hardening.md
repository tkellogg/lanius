---
status: planned
author: Claude Opus 4.8 (planner)
last-updated: 2026-07-07
---

> **Known-failing (pre-existing, environmental — not a refresh regression).** Two
> assertions in the AI-panel flow time out reproducibly on this local harness:
> - `ui.spec.mjs:2621` — "ai panel: a helper navigate call switches the view"
> - `ui.spec.mjs:2626` — "ai panel: the tool call/result round-trips over
>   `obs/agent/helper/<session>/tool/*`"
>
> Both wait for an **externally-emitted** bus event
> (`lanius emit obs/agent/helper/<session>/tool/navigate/call`) to travel the
> daemon → SSE stream → browser and drive the SPA. Confirmed pre-existing by
> running the **pre-refresh base** (`/Users/tim/code/elanus`, `elanus` binary
> built 2026-07-06, before any refresh edit): it fails these **two identically**
> (base result: 294 ok / 3 FAIL — these two plus the sanctioned `:1948` flake).
> The SSE wiring is untouched by the refresh: `ui/web/src/live.ts` is
> byte-identical to base, and `AgentAssistant.tsx`'s tool-call subscription is
> intact (its only diff is a `<label>` word `profile`→`agent`). The same
> `#view-providers` selector these check reaches PASSES via a nav click in the
> same run — the gap is only the daemon's delivery of an external `emit` into the
> live SSE stream. These are **outside the sanctioned modulo below** and should
> be treated as known-failing until the obs-bus SSE-delivery limitation of the
> local harness is fixed; no SPA/refresh code change addresses them.

# E2E flaky-test hardening — and the two real app bugs hiding behind them

The Playwright suite `ui/web/test/ui.spec.mjs` (~138 assertions,
`node test/ui.spec.mjs`) has three intermittent failures. Grounded root-causing
found that **two of the three are genuine application bugs** (they affect real
users, not just the test), and that a thrown click currently **aborts the entire
suite** rather than failing one assertion. This handoff fixes both the app bugs
and hardens the harness.

**Do the suite-wide wins (M1) first** — two ~one-line changes that remove whole
classes of flakiness and stop the run-aborting behavior.

---

## M1 — suite-wide hardening (highest leverage, do first)

1. **Disable animations in the test context.** `styles.css:1272` already has a
   `@media (prefers-reduced-motion: reduce)` block that nulls the `.mast/.panel/
   .stage` `settle` entrance animation (0.7–0.86s) and the message `arrive`
   animation — but the test never activates it. `browser.newContext(...)`
   (`ui.spec.mjs:66`) doesn't request it. **Add `reducedMotion: 'reduce'`** to
   that `newContext` call. Every "element is not stable" window caused by an
   entrance animation disappears in one line.
2. **Make `waitFor` not abort the suite.** `waitFor` (`ui.spec.mjs:26-34`) has
   **no try/catch** around `await fn()`. Several callbacks call `.click()`
   internally (e.g. `:268,:529,:543,:1934`); any actionability timeout there
   throws an **uncaught exception that crashes the whole run** (this is why flake
   3 presents as a "suite abort," not a clean `FAIL:`). **Wrap `fn()` in
   try/catch** — treat a throw as "not true yet, keep polling," so a single bad
   assertion degrades to one `FAIL:` line instead of taking down the suite.

- **Acceptance:** `node test/ui.spec.mjs` run 3× consecutively never aborts
  mid-suite; any residual failure is a clean `FAIL:` line.

## M2 — the real application bugs (fix these for users, not just the test)

### 2a. `loadConfigure` stale-response race → "cost summary follows second agent"
- **Bug:** `loadConfigure` (`App.tsx:996-1063`), fired by the sel-change effect
  (`App.tsx:849`), is a bare `async` with **no request-generation guard**. It
  awaits several `adminGet` round-trips then unconditionally
  `setCfgForm/setCfgParsed/…`. Switching agents while a slower load (e.g.
  `harrier`, with more shared-config fan-out, `:1044-1051`) is in flight lets the
  **stale response overwrite the newer agent's rendered config** — the summary
  shows the wrong agent. `costSummary()` (`:2112`) is derived synchronously, so
  the only way the assertion (`ui.spec.mjs:552-562`) fails is this race.
- **Fix:** capture a generation counter (or check
  `agentName === selRef.current.agent`) before applying the setters; discard
  stale results. (Test-side: bump the `5000ms` timeout to `8000ms` to match its
  siblings at `:568,:574`, and read the four values in one `page.evaluate`
  snapshot instead of four `$eval` round-trips.)

### 2b. `.nav-item` flex-truncation → stuck `scrollLeft` → "compose input clipped"
- **Bug (reproduced with certainty; measured `left: -41` at failure):** a long,
  decorated agent name (e.g. `"AR architect ·live"`, the `.nav-live` badge at
  `App.tsx:1669`) overflows the nav drawer because `.nav-item`
  (`styles.css:241`, `display:flex`) never sets `min-width: 0`, so
  `.nav-agent-name`'s `text-overflow:ellipsis` can't engage (classic flex
  min-content bug). Clicking the overflowing item makes Playwright auto-scroll
  it into view, leaving `.app-shell` with a nonzero `scrollLeft`; because
  `.app-shell` is `overflow:hidden` (`styles.css:146`) the scroll **silently
  persists** (clips instead of showing a bar) and **nothing ever resets it**, so
  a later narrow-view check (`ui.spec.mjs:1948-1954`) sees the compose input
  shifted off-screen. Intermittent because it depends on the first agent's name
  length.
- **Fix (real narrow/mobile bug):** add `min-width: 0` to `.nav-item` (or
  `flex:1; min-width:0` on the `.nav-agent-name` flex context) so names
  truncate; and/or defensively reset `.app-shell.scrollLeft = 0` on every
  tab/agent switch (the sel-change effect, `App.tsx:847-857`). (Test-side: reset
  `scrollLeft` before measuring, or assert relative to the container.)

### 2c. `helperRunnable` banner pops under the cursor → providers-link "not stable"
- **Bug:** `setup-chat-offer` renders conditionally on
  `helperRunnable = llm.world === 'a'|'b'` (`App.tsx:1738,1806-1812`), directly
  **above** the wizard holding `[data-providers-link]` (`primitives.tsx:94`,
  `App.tsx:1814-1833`). `systemStatus` is replaced wholesale by both the 10s
  `loadSystemStatus` poll **and** `loadSetup` itself (`App.tsx:883`); after the
  flow adds providers, `llm.world` can flip between the first render and
  `loadSetup`'s refetch, making the banner appear/disappear and **shift the link
  vertically** just as Playwright clicks it (`ui.spec.mjs:2409`, a bare
  `await naLink.click()` on a raw handle → "not stable / intercepts pointer
  events"). With M1's `waitFor` fix this stops aborting the suite, but should be
  made stable.
- **Fix (test-side, primary):** use a **locator**
  (`page.locator('#view-setup [data-providers-link]').click()`) instead of the
  stored `page.$` handle — locators re-query and retry against the current
  position. (App-side, minor UX: don't let `loadSetup`'s status refetch override
  an unchanged `helperRunnable`, or don't place volatile content above stable
  interactive controls.)

- **Acceptance (M2):** each of the three assertions passes across 5 consecutive
  full-suite runs; the two app-side fixes (2a race guard, 2b truncation) are
  verified by their own targeted checks, not just the flaky assertion.

## M3 — residual test-hygiene (optional, low priority)
- Convert the other stored-raw-handle click (`cfgLink`, `ui.spec.mjs:2432`) to a
  locator for the same reason as 2c (lower risk — it reaches
  `data-providers-link` via the Configure tab, not through the shifting Setup
  banner).
- **Acceptance:** no remaining `page.$(...)` handle stored across an `await` gap
  before `.click()`.

---

## Wonky bits
1. **Two of these are app bugs worth fixing regardless of the test** (2a config
   race, 2b long-name overflow → both hit real users). Don't "fix" them purely
   test-side and move on.
2. **M1 alone may make all three pass** by removing the animation-stability
   window and stopping the abort — but the app bugs remain latent. Do M2 anyway.
3. **Flakiness is load-dependent** — verify fixes with several consecutive runs,
   not one green pass.

## Read these first
- `ui/web/test/ui.spec.mjs` — `waitFor:26`, `newContext:66`, the three
  assertions `:552`, `:1948`, `:2404`.
- `ui/web/src/App.tsx` — `loadConfigure:996`, sel-effect `:847`,
  `helperRunnable:1738`, `setup-chat-offer:1806`, `loadSetup:883`,
  `loadSystemStatus:552`.
- `ui/web/src/styles.css` — `.app-shell overflow:hidden :146`, `.nav-item :241`,
  `.nav-agent-name :206`, the reduced-motion block `:1272`.
- `ui/web/src/components/primitives.tsx:94` — the providers-link render site.

## Log
- 2026-07-07 (Opus, planner): root-caused from a grounded research pass (all
  three reproduced or code-confirmed). Headline: two are real app bugs
  (`loadConfigure` stale-response race; `.nav-item` flex-truncation leaving
  `.app-shell.scrollLeft` stuck), and `waitFor`'s missing try/catch turns any
  actionability timeout into a whole-suite abort. Highest leverage is M1's two
  one-line changes (`reducedMotion:'reduce'` + guard `waitFor`), which kill an
  entire class of animation flakiness and stop the aborts; M2 fixes the
  user-facing bugs behind 2a/2b.
