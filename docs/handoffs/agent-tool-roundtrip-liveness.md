---
status: done — M2A (test injects via live /api/publish; the 2 AI-panel round-trip assertions pass). M2B kernel-broaden not needed.
author: Claude Opus 4.8 (planner)
last-updated: 2026-07-07
---

# Agent client-tool round-trip: making tool events reach the browser live

**The felt problem.** The helper AI panel (agentic-configuration M2) lets the
agent call *client tools* the browser executes — `navigate`, `get_status`, etc.
The agent emits a tool **call** on the bus; the browser SSE stream sees it, runs
the tool, and publishes the **result** back. This works for a real in-process
turn but **silently does not work when the tool event is written via
`lanius emit`** — which is exactly how the e2e injects it, and possibly how a
*harness-backed* helper turn (M4) would relay it. This handoff pins the gap and
decides the fix.

Surfaced during helper M2/M3 verification: the e2e assertion
`ai panel: a helper navigate call switches the view` times out.

---

## How the round-trip works (grounded)

1. **Browser side** — `ui/web/src/components/AgentAssistant.tsx` opens a live
   SSE stream (`openLiveStream` → `GET /api/stream`, `AgentAssistant.tsx:98`)
   and matches bus topics
   `obs/agent/<noun>/<session>/tool/<callId>/(call|result|await)`
   (`AgentAssistant.tsx:38`). On a `call` it runs the client tool and publishes
   the `result` back (`:122-133`).
2. **Server side** — `src/web.rs` subscribes **`obs/#` live** on the bus
   (`web.rs:317`) and fans each received frame out to every SSE client
   (`web.rs:97` `broadcast`, `/api/stream` at `:215`/`:254`). So **anything
   live on the bus under `obs/agent/.../tool/...` reaches the browser.**
3. **The agent's tool call must therefore be LIVE on the bus.** A real
   in-process turn publishes its obs live (bus-origin) — that frame is written
   to the ledger already `announced=1`, and it also fans out to subscribers in
   real time. The browser sees it. ✅

## The gap (grounded)

`lanius emit <topic>` writes an event to the **ledger** with `announced=0`; it
does **not** publish live. The only path from ledger→live bus is
`announce_ledger_events` (`src/dispatcher.rs:290`), which re-announces **only**
events whose type starts with `in/`, `signal/`, or `obs/config/`
(`dispatcher.rs:310-313`). Its own comment is explicit: *"Other obs/ types (e.g.
`obs/channel` receipts via `lanius emit`) keep their … ledger/emit echo only."*

So an `emit`'d `obs/agent/<noun>/<session>/tool/<callId>/call`:
- is ledgered (`announced=0`),
- is **not** in the re-announce allow-list,
- therefore **never reaches the web server's `obs/#` live subscription**,
- so the browser SSE never sees it and the client tool never runs.

That is precisely why the e2e's `lanius emit`-injected `navigate` call times out.
It is **not** a bug in the M2 UI — the UI is correct; the *event never arrives*.

## Why the fix choice is non-trivial (the M4 tie-in)

There are two clean fixes, and which is right depends on a fact the implementer
must confirm first:

**Does any REAL (non-test) helper turn deliver tool events via `emit`/ledger
rather than a live publish?**

- If helper turns always run **in-process and publish obs live**, then nothing
  real is broken — only the *test* uses a non-live injection, and the fix is to
  make the test inject live (Fix A).
- But a **harness-backed helper turn (agentic-configuration M4)** runs the turn
  through a headless coding worker whose obs are **captured and relayed** by the
  parent — that relay may go through the same ledger/emit echo, in which case a
  real harness-backed helper's `navigate` call would **also** fail to reach the
  browser, and the dispatcher must learn to announce it (Fix B). This handoff
  and the **Helper M4** handoff are coupled here — settle them together.

---

## Milestones

### M1 — Determine the real delivery path (grounding gate)
- Trace how a helper turn's `obs/agent/.../tool/.../call` reaches the bus in
  each mode: (a) in-process/provider turn, (b) harness-backed worker turn (M4,
  if/when it exists — today check the nearest analog: how a headless code
  worker's obs are relayed to the live bus vs only ledgered).
- **Acceptance:** a written determination — "real tool calls are always
  live-published" (→ Fix A only) OR "the relayed/worker path only ledgers them"
  (→ Fix B needed). Cite the publish call sites.

### M2A — Make the e2e inject like production (always do this)
- Change the `navigate`/client-tool assertions in
  `ui/web/test/ui.spec.mjs` to inject the synthetic tool `call` via a **live bus
  publish** (e.g. `POST /api/publish`, or a `lanius bus pub` if one exists, or a
  direct MQTT publish on the test broker) instead of `lanius emit`, so the test
  exercises the real SSE path. Confirm `openLiveStream`/`/api/stream` then
  delivers it and the view switches.
- **Acceptance:** the `ai panel: a helper navigate call switches the view` (and
  its dependent `tool call/result round-trips`) assertions pass deterministically.

### M2B — Announce agent tool calls to the live bus (only if M1 says a real path ledgers them)
- Narrowly extend `announce_ledger_events` (`dispatcher.rs:310`) to also
  re-announce `obs/agent/<noun>/<session>/tool/<callId>/call` and `/await`
  (the agent→browser direction) — **not** `/result` (browser→agent). Keep the
  match tight (this exact tool-call topic shape), not a blanket `obs/agent/#`.
- **No double-publish risk:** a bus-origin event is already `announced=1` and
  never enters this sweep (per the existing comment), so this only affects
  events that were *only* ledgered — exactly the relayed/worker case.
- **Acceptance:** a harness-backed (or otherwise relay-delivered) helper turn's
  `navigate` call reaches the browser and switches the view; the CLI/`emit`
  semantics for other `obs/` types are unchanged; `cargo test` green.

---

## Wonky bits / decisions

1. **Prefer Fix A; add Fix B only if M1 proves a real path needs it.** Do not
   broaden kernel announce behavior to satisfy a test — but do broaden it if a
   real harness-backed turn genuinely relays tool calls through the ledger.
2. **Scope the topic match tightly (Fix B).** Announcing all `obs/agent/#`
   would put every agent observation on the live bus, changing the volume and
   meaning of the live stream. Only the `tool/<id>/call` + `/await` shape needs
   liveness for the round-trip.
3. **Direction matters.** `call`/`await` flow agent→browser (need liveness);
   `result` flows browser→agent via `/api/publish` (already live). Don't
   re-announce `result`.

## Read these first
- `src/dispatcher.rs:290` `announce_ledger_events` — the re-announce allow-list.
- `src/web.rs:317` (live `obs/#` subscribe) + `:97` broadcast + `:215`/`:254`
  `/api/stream` SSE — how the browser is fed.
- `ui/web/src/components/AgentAssistant.tsx:38` (topic match) + `:98`
  (`openLiveStream`) + `:122-133` (run tool, publish result).
- `ui/web/test/ui.spec.mjs` — the failing `navigate`/round-trip assertions and
  their `lanius emit` injection.
- The **Helper M4** handoff (harness-backed turns) — coupled via M1.

## Log
- 2026-07-07 (Opus, planner): wrote the handoff after finding the round-trip
  gap during helper M2/M3 verification. Grounded the path browser↔SSE↔bus and
  the `emit`→ledger dead-end. Key call: the fix is *probably* test-only
  (production publishes live), but it is genuinely coupled to Helper M4 — a
  harness-backed turn that relays tool calls through the ledger would need the
  dispatcher to announce them, so M1 is a grounding gate before choosing A vs B.
