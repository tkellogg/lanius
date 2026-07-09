---
status: done
author: Fable 5 (planner) under Fable, for Tim
last-updated: 2026-07-08
---

# Handoff: no more dead air in chat (H2)

Three times in the 2026-07-08 walkthrough Tim typed a message and got
*nothing* — no acknowledgment, no failure, no next step
(docs/journeys/07-chatting.md, "Dead air is the one unforgivable failure").
The mechanics: compose → `submitCompose` (App.tsx:1444-1475) → POST
/api/publish → web.rs `publish()` enqueues onto the broker and returns
`{ok:true}` with no subscriber check (src/web.rs:382-393). The only feedback
is the send button flashing "accepted ✓" for 1.4 seconds (App.tsx:1472-1474).
If the background service is down, the message sails into the void and the
thread shows nothing, forever.

This handoff fixes the MAIN chat (ConverseView). The helper panel has the
same disease but is owned whole by helper-first-encounter (H4), which
consumes the health hook built here.

## Dependency edges

- Requires app-tsx-split (H0) committed — this edits `views/ConverseView.tsx`
  and App.tsx's loaders, post-split.
- Gates package-truth (H3) and helper-first-encounter (H4): both consume the
  `useSystemHealth` hook this handoff creates.

## Read these first

1. docs/journeys/07-chatting.md — especially "Dead air" and "What chatting
   should feel like". The implementation is judged against this doc.
2. App.tsx `submitCompose` (1444-1475), `onLiveMessage` (1330-1398),
   `addFailure` (1407-1412), `corrAgent`/`corrSession` refs (557-558).
   (Post-split these live in App.tsx still; line numbers will have shifted.)
3. src/web.rs `publish()` (355-393) and `status()` (466-510) — what the
   server can and cannot know.
4. ui/web/src/live.ts:36-73 — the SSE stream every state transition rides.

## Wonky bits / decisions (already made)

1. **The broker cannot confirm delivery — the UI must not pretend.** Publish
   is fire-and-forget by design (topic plane, no subscriber introspection).
   The honest vocabulary is *sent* / *waiting* / *no response yet* — never
   "delivered". Put the honesty in the copy: the stalled state says "No
   response yet. The agent may not be running." — a statement about what we
   can see, not a diagnosis we can't make.
2. **Pending state is keyed by `corr`, NOT by the open thread.** The
   reply-attribution logic (App.tsx:1357-1363) only renders replies into the
   open thread when the session matches; a pending indicator keyed to "the
   open conversation" would strand when the user switches threads or when an
   event-triggered reply lands elsewhere. Keep a `Map<corr, {state, sentAt,
   session, agent}>` ref in App; ConverseView renders indicator state for the
   messages it shows by their `corr`.
3. **The three-state machine, with real signals:**
   - **sent** — set locally the moment `addConv` echoes the message
     (App.tsx:1457). Shown as a subtle per-message "sent" mark.
   - **thinking** — the first `obs/agent/<agent>/<session>/…` SSE event whose
     session matches the corr's session (via `corrSession`, App.tsx:1389)
     flips it. This is a true "the agent woke up" observation. Render as a
     thinking indicator (animated dots row) at the bottom of the thread.
   - **resolved / stalled** — a correlated `in/human` reply, ask, or
     failure-mail resolves it (existing paths: 1357-1363). If 20 seconds pass
     with NO obs activity and no reply → stalled: an in-thread status line
     "No response yet. The agent may not be running." with two affordances:
     **check status** (navigates to the setup/status view) and **retry**
     (re-publishes the same text with a fresh corr; the old pending entry is
     dropped). 20s is a constant, not config.
   - A reply that arrives AFTER the stalled line replaces it (the pending map
     entry resolves; the status line unrenders). Late is fine; lost is what
     we refuse to hide.
4. **Send-time pre-check — don't send into a known void.** Before publishing,
   consult health (below): if `broker_connected === false` or
   `llm.world === 'c'`, skip the fake-optimism entirely — render the
   in-thread failure line immediately ("Nothing is running that can answer
   this yet") with a link to setup. Mirrors the existing world-c guard on the
   helper panel (App.tsx:1693-1697).
5. **`useSystemHealth` — the shared hook, built here, consumed by H3/H4.**
   One module (`ui/web/src/lib/health.ts` + a hook) that exposes the already-
   polled facts as one object: `{ brokerConnected, llmWorld, historyAvailable,
   commsAvailable, actorStatus(name) }`, sourced from the existing
   `systemStatus` (App.tsx:598-601, polled every 10s at 624) and `liveness`
   (App.tsx:603-606) state — no new server endpoint, no new polling loop.
   The hook takes those as inputs (or App passes the object down); it is a
   projection, not a fetcher.
6. **The failure path already exists — reuse it.** Real failure-mail renders
   via `addFailure` (App.tsx:1407) with the `fail-hint` copy in ConverseView
   (App.tsx:2709). The stalled line is a *different* class (we don't know it
   failed) — style it as uncertainty, not error.
7. **The empty state invites** (07-chatting.md last paragraph): the empty
   ConverseView copy (App.tsx:2708) becomes an invitation to say hello, and
   saying hello must visibly do steps 1-3 above. Copy change only; keep it
   to one warm sentence.
8. **Language rules:** "conversation", never "session"; no raw corr ids in
   copy (the existing `title={m.corr ...}` tooltip may stay).

## Milestones

### M1 — the health module

`lib/health.ts`: the projection described in wonky bit 5, unit-testable as a
pure function of (status, liveness) inputs.

**Acceptance:** a vitest/unit test (or, if no unit runner exists, an
exported pure function exercised from ui.spec.mjs) covering: broker down,
world c, history unavailable, actor running/failed/not-started.

### M2 — per-message pending + thinking + stalled

The corr-keyed machine (wonky bits 2-3) in App.tsx; indicator rendering in
`views/ConverseView.tsx`.

**Acceptance (drive the real app, re-embedded binary):**
- With the daemon RUNNING: send a message → the message shows "sent"; a
  thinking indicator appears once obs events flow; the reply lands and both
  marks resolve. (ui.spec.mjs already has the reply plumbing to extend.)
- With the daemon DOWN (but web up): send → within ~20s the thread shows the
  "No response yet…" line with working check-status and retry affordances —
  nothing silently vanishes. Retry after starting the daemon produces a
  reply.
- Switching to another conversation and back does not duplicate or strand
  indicators (corr-keyed, wonky bit 2).

### M3 — send-time pre-check + inviting empty state

Wonky bits 4 and 7.

**Acceptance:** with the broker disconnected, sending renders the immediate
in-thread "nothing is running" line with a setup link (no 20s wait); the
empty conversation shows the invitation copy; full ui.spec.mjs green.

## Log

- 2026-07-08 — planned (Fable 5 under Fable). Scope call: helper panel's
  dead-air moved to helper-first-encounter (same files as the helper
  rebuild); this handoff owns ConverseView + the shared health hook.
- 2026-07-09 — implemented (M1-M3): lib/health.ts projection + hook, the
  corr-keyed pending machine in App, indicators + stalled/no-path lines in
  ConverseView, invite empty state; verified by ui.spec.mjs flow 6f (25
  assertions, full suite 331 green) and a manual daemon-up/daemon-down
  observation run. Note for the suite's caretaker: one full-suite run crashed
  environmentally at the pre-existing ambient flow (ui.spec.mjs ~1352 — a
  Playwright click auto-wait TimeoutError escapes waitFor and aborts the whole
  run) while orphaned stacks from an earlier interrupted run were alive;
  identical code passed clean. Not fixed here — outside this handoff's scope.
- 2026-07-09 — VERIFIED (adversarial Opus, fresh context; GPT-5.5 channel
  unavailable — codex workers SIGKILLed in this environment): pass=true,
  build/tests ok, 331/331 e2e, live-stack probes confirmed (daemon-down
  immediate no-path, ~20s stall + retry, resolution clears indicators),
  scope clean (exactly the six allowed files). Special-attention verdicts:
  corr-machine survives thread switches; no timer stacking; 306 baseline
  intact; no smuggled changes.
- 2026-07-09 — fix round 1 (same implementer, bounded): (1) retry
  double-click race closed via consume-once retriedCorrs ref — e2e proves
  exactly one publish on double-click; (2) correlated obs after stall now
  lifts the stalled line back to thinking — e2e stalls ~20s, publishes a
  real obs event, observes the flip, then a reply resolves. tsc clean,
  fresh embed, full suite 336/336 ALL PASS. Fix touched only App.tsx +
  ui.spec.mjs.
- 2026-07-09 — residual nits (triaged by Fable, deliberately NOT fixed):
  stall timers have no unmount cleanup (App is root-lifetime, harmless);
  clearPendingTimer runs inside the setPending updater (idempotent,
  style); never-resolved pending entries persist for the session (bounded
  by sends); duplicate no-path notices dedupe to one line (pre-existing
  message keying). Also for the suite caretaker: pre-existing structural
  flake at ui.spec.mjs ~1352 (see implementer note above).
