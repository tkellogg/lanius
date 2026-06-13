# ui/web testing plan

Three layers, cheapest first. A bug should be caught at the lowest layer
that can see it; the browser layer exists for what only a browser can see
(dead handlers, broken rendering, real user flows).

## 1. API smoke — `npm test` (test/smoke.mjs, exists)

Real daemon on a throwaway root, the server as its MQTT client, plain
HTTP as the browser. Covers: SSE relay, publish→ledger with correlation,
ring catch-up, history proxy in both states + all query kinds +
pagination + the search DSL, and the whole admin seam — kits
list/readme/stage, pending queue, profile read/write + traversal guard,
agents list/create/set (incl. the array round-trip regression and
invalid-set-refused), approve with decided_by=ui, CSRF/DNS-rebinding
guards, models fallback, seeded kits. Fast, no browser, runs everywhere.

## 2. Browser e2e — `npm run test:ui` (test/ui.spec.mjs, Playwright)

Headless Chromium against the same kind of live stack. NOT part of
`npm test` (the chromium download is heavy; CI/dev opt in). Asserts the
flows a human actually performs:

- boot: nav shows disk agents on a silent root; signals view renders
- new agent: form → agent appears in nav → configure tab opens
- configure: edit model/turns/include → save → note says saved →
  reload → values persisted (the exact layer that broke: form→server
  value encoding)
- rename: agent field → save → nav updates, selection follows
- kits & review: catalog lists seeded kits, readme expands, stage →
  pending queue fills → approve button → queue drains, badge flips
- converse: compose → message appears in the feed (round trip over the
  real bus)
- degraded history: hint visible without the history package approved
- no console errors on any visited view (page error listener)

## 3. Manual / exploratory — scripted walkthrough + screenshots

The Playwright driver doubles as the manual pass: a walkthrough script
screenshots every view (signals, each agent tab, kits & review, blank
root vs populated) into /tmp/elanus-ui-shots/ for human review. Taste
problems (layout, copy, affordances) are found by looking, not asserting
— Tim reviews the shots, files feedback, the loop repeats.

## Invariants worth keeping

- tests/e2e.sh stays node-free; ui/web owns its node tests.
- Every admin mutation asserted at layer 1 before layer 2 uses it.
- A layer-2 failure that layer 1 could have caught gets a layer-1 test
  in the same commit.
