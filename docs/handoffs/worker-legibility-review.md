---
status: changes-requested
author: Codex (code-81bdabb3)
last-updated: 2026-07-12
reviewed-scope: chainlink #8 working tree
---

# Worker run clarity review

## Verdict

Changes requested. The projection, launch-edge storage, chat-first navigation,
and copy rename are directionally sound, and the current tree passes all 631
Rust library tests plus the TypeScript/Vite build. Two acceptance-level problems
remain: normal interactive launches can promote a harness flag as the run's
purpose, and History's new three-state status does not reach the React UI.

Reviewed changes:

- `src/code_projection.rs`, `src/codeagent.rs`, `src/codesession.rs`, `src/db.rs`
- `src/web.rs`
- `ui/web/src/App.tsx`, `CodeSessions.tsx`, `views/Nav.tsx`,
  `views/SessionsView.tsx`, `views/WelcomeView.tsx`
- `ui/web/test/ui.spec.mjs`
- `docs/handoffs/worker-legibility.md`

Excluded: the broker refusal-log patch and the separate `messages-legibility`
branch (chainlink #12).

## Findings

### High: interactive harness flags become the run's displayed purpose

`src/codeagent.rs:3852` defines the launch prompt as every remaining harness
argument joined with spaces. At `src/codeagent.rs:3900`, any non-empty value is
stored as the session intent and prevents the first real user prompt from
seeding it later. The Runs UI then promotes that value to identity text and the
detail heading.

This is visible in current live data: Fable's session `code-85666abd` has intent
`--dangerously-skip-permissions`, not the work Fable is doing. The new browser
test does not cover the capture path; it inserts idealized intent directly into
SQLite (`ui/web/test/ui.spec.mjs:2584`).

Impact: the feature's primary promise, “show what this worker is doing,” can
confidently show a launch flag instead. Because `set_intent_if_absent` sees a
non-empty baseline, a later real prompt cannot repair it.

Fix: derive purpose from the parsed task/prompt for headless launches, not from
the complete forwarded harness argv. For an interactive launch with only flags,
record no baseline and let the first user-prompt hook seed it. Add adapter-level
tests for interactive Claude/Codex/OpenCode flags and a headless prompt.

### High: History's three states are not wired into the UI

`src/web.rs:535` emits `history.state` as `reachable`, `unreachable`, or
`absent`, but `ui/web/src/App.tsx:220` only stores the status response in
`systemStatus`. The UI's `historyOk` value is still independently set from the
periodic History query at `ui/web/src/App.tsx:230`, and neither Nav nor Welcome
reads `systemStatus.history.state`.

Both unavailable states therefore render the same instruction at
`ui/web/src/views/Nav.tsx:82`: “Start or approve it.” That is wrong for a package
which is approved but unreachable, and it misses the handoff's explicit
three-state acceptance clause.

Impact: the backend does an extra liveness probe and exposes a richer wire shape,
but the person still cannot tell “not installed/running” from “configured actor
is down.” M3 is incomplete end to end.

Fix: make the status response the shared History availability source, retain the
state word in React, and render distinct absent/unreachable repair copy. Keep the
actual History request as the data load, not as a second liveness state machine.
Add browser assertions for all three states.

### Medium: an unusable History endpoint can be reported reachable

`src/web.rs:2288` classifies every HTTP status below 500 as reachable. The probe
sends a valid query to the expected `/query` endpoint, so 401, 403, 404, 409, or
429 all mean History is not currently usable by this web client even though a
server answered.

Impact: `/api/status` can say `available: true` while the same operation the UI
needs would fail.

Fix: define reachability as a successful query response (normally 2xx), or parse
the actor's documented success envelope. Add 401 and 404 cases to
`classify_history_three_states`.

### Medium: the History cache does not coalesce concurrent probes

`src/web.rs:563` checks the cache, releases the mutex, performs the HTTP request,
then writes the result. Multiple status requests arriving after expiry can all
observe an empty/stale cache and issue probes concurrently. The current test only
calls the pure cache reader with a pre-populated value; it does not exercise the
acceptance requirement that polling produces at most one probe per cache window.

Impact: a burst of `/api/status` requests can still hammer a slow or failing
History actor, which is the behavior the cache was added to prevent.

Fix: store an in-flight shared future/result, serialize refreshes, or use a
single-flight cache primitive. Test concurrent callers against a counting stub.

### Medium: raw run IDs still lead the primary list row

The handoff says purpose becomes the primary identity and the raw ID becomes
secondary. In `ui/web/src/CodeSessions.tsx:144`, the raw `code-*` ID is still the
first field in every top-level row, before intent. It retains the stronger
existing ID styling, while intent is a later truncated field. Child rows do
reverse that hierarchy, so the same concept is presented inconsistently.

Impact: the Runs list still opens with the opaque identifier Tim called out in
dogfood, even when a useful purpose exists.

Fix: use intent as the first/primary label whenever present and move a shortened
ID to secondary text or an agent-reference control. Keep the full ID in a
tooltip/detail view. Add a visual-order assertion, not only a `textContent`
contains check.

## Test and design notes

- `cargo test --lib`: 631 passed, 0 failed.
- `npm run build`: TypeScript and Vite passed. The existing bundle-size warning
  remains.
- `git diff --check` passed for the reviewed files.
- M1's UI fixture proves rendering from idealized rows, but not real launch-to-
  projection behavior. This is why it misses the polluted-intent finding.
- M2 tests cover env classification, storage round-trip, and projection backfill,
  but not an actual detached spawn producing both the durable edge and
  `session/start.launched_by_event`. Add one integration-level assertion before
  calling M2 fully verified.
- The chat-first change is small and composes with the existing trace fallback:
  worker navigation enters Chat while conversation loading resolves, then falls
  back to the trace view when no conversation exists. Existing browser checks
  cover both worker-DM paths.
- The visible Converse-to-Chat rename is appropriately scoped; internal route and
  tab identifiers remain stable.

## Suggested order

1. Fix purpose extraction and test real launch paths.
2. Finish History state wiring, tighten classification, and make cache refresh
   single-flight.
3. Reverse the run-row identity hierarchy.
4. Run the focused browser suite, then the planned independent verifier.
