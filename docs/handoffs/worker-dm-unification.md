---
status: proposed
author: Claude Opus 4.8 in Claude Code on Elanus (session code-543b576e)
last-updated: 2026-07-09
---

# Handoff: unify worker-session DMs into the chat plane

A coding session that messages its owner is invisible in the web UI. Live repro
(2026-07-09): session `code-543b576e` emitted `in/human/owner` (event 5411,
correlation `code-deliver-92da…`) replying to an owner delivery. The OS
notification fired (the notifier package watches the bus and doesn't care who
the sender is), but the message appears in **no** web-UI conversation. The
owner's question — "where would it even be?" — has no good answer today.

## Why it's invisible

The conversation-list projection (relocated to the comms package,
`kits/stdlib/packages/comms/scripts/comms_view.py`) drops worker sessions in
both of its paths:

- `comms_view.py:311` (inbound seeds): *"Worker (coding-run) sessions stay in
  the trace view, not chat"* → `if is_worker_session(conn, session): continue`
- `comms_view.py:347` (ambient agent-first sends): same filter.

So agent-DM-in-the-web-UI exists **only for native agents** — exactly the gap
the 2026-07-08 channels/routing audit flagged. The trace/session view shows the
coding run's *activity*, but the human-facing DM exchange (owner delivers a
message, worker replies on the correlation) never assembles into a thread
anywhere.

This is also the reference violation of the simple-core law
(`docs/channels.md`, closing section): `is_worker_session` /
`WORKER_PREFIX = "code-"` hard-code the channel taxonomy into the projection.
Adding or reclassifying a channel edits code, not a descriptor.

## Goal

A coding session's exchange with the owner threads as a first-class
conversation in the web UI, like any agent DM — and the projection stops
learning channel names from string prefixes.

**Explicitly NOT in scope — the security core.** The `code-` prefix is
load-bearing in the broker ACL (`broker.rs:440`); worker authority, grants, and
mailbox scoping do not change here. This is a **read-model + UI** unification.
If a milestone finds itself editing broker/ACL code, stop — wrong seam.

## What a worker conversation IS (and isn't)

The thread is the **DM exchange only**, not the coding trace:

- owner → worker: `code-deliver` events (`lanius code deliver`, web
  `/api/code/deliver`, correlation `code-deliver-<uuid>`)
- worker → owner: `in/human/<owner>` events whose broker-verified sender is
  that worker session (ambient), or whose correlation joins a delivery.

Tool calls, file edits, sub-worker spawns stay in the trace/session view. The
session detail page and the chat thread should cross-link (chat header → trace
view; trace view → conversation panel), not duplicate each other.

## Milestones

### M1 — projection: worker sessions become conversations

In `comms_view.py`, replace the two `continue`s with a fold that builds a
conversation from the DM exchange:

- Seed from `code-deliver` traffic and from `in/human/<owner>` rows whose
  sender is the worker session (the ambient path already requires
  broker-verified `sender == agent`; reuse that discipline — sender is
  broker-verified, never payload-claimed).
- Correlation join: a `code-deliver-*` correlation groups the owner's delivery
  and the worker's reply into one thread, same as the existing prompted-thread
  join.
- The conversation row carries an honest source chip (e.g. "coding session")
  and the session's note/title where one exists (`lanius code note`).
- Decide list placement: same list as agent DMs, distinguishable by chip —
  NOT a separate silo (a silo recreates the taxonomy split one level up).

### M2 — reply-from-chat routes to the worker

Replying inside a worker conversation must route via the deliver path
(`/api/code/deliver`, which the walkthrough sprint already built), not the
native-agent exec path. The UI seam exists (`ui/web/src/lib/conversation.ts`
knows deliver is a worker-only affordance); wire it so one compose box does the
right thing per conversation kind, driven by data the projection returns — not
by the client re-deriving "is this a worker" from the session id.

Tie-in: the `inbox-provenance` branch (in flight, 2026-07-09) reworks how
delivered messages render inside the worker's context — fenced, full-verbatim,
harness-asserted provenance. A chat-sent reply should arrive through that same
rendering. Verify the round-trip end-to-end: web chat → deliver → worker inbox
→ worker reply → web chat.

### M3 — kill the taxonomy special-case (the simple-core payoff)

Move the hard-coded channel taxonomy out of the projection:

- `source_for` already prefers an explicit stamped `payload.source`; make the
  emit paths stamp it (the deliver path, `codeagent.rs` reply emission) so the
  hard-coded fallbacks become dead weight.
- Relocate the remaining fallback taxonomy (`web-`, github/jira/linear, cron,
  `code-`) into channel descriptors the comms package reads from config/package
  data, per `docs/channels.md`: "the kernel should never learn a new channel's
  name." `withheld_builtin_tools` (`src/packages.rs`) is the pattern done right.
- `is_worker_session` survives only where it reflects a real durable fact (the
  stored `kind` in `code_sessions`), not as a display-routing switch.

### M4 — tests + e2e

- Python-side: projection tests for the worker-conversation fold, the
  correlation join, and the sender-verification rule (a payload claiming
  `session: code-x` from a non-matching sender must NOT create/join a thread —
  that's a spoof vector).
- `ui.spec.mjs`: a worker DM thread renders; reply-from-chat delivers; the
  chip and cross-links are present.
- `cargo test` for any Rust-side seam (deliver stamping, conversation-detail
  reads in `web.rs`).
- Build ritual: `npm run build` → cargo build (build.rs embed-freshness now
  handles staleness) → run e2e against the Rust server.

## Acceptance

1. From a coding session: `lanius emit in/human/<owner> --payload '{"text":…}'`
   (or a correlated reply to a deliver) → the message appears in a web-UI
   conversation, live over SSE, attributed to that session with an honest chip.
2. Replying in that conversation lands in the worker's inbox with full
   provenance rendering; the worker's next reply threads back. One
   conversation, both directions.
3. `grep -n 'WORKER_PREFIX\|is_worker_session' kits/stdlib/packages/comms/` —
   no display-routing use remains; taxonomy lives in descriptors.
4. Broker ACL (`broker.rs`) untouched — diff shows zero security-core changes.
5. Full `cargo test` + `ui.spec.mjs` green; report counts.

## Context for the implementer

- `docs/channels.md` — the conversation model + the closing "principle, made
  concrete" section this handoff executes.
- 2026-07-08 audit conclusion: no transport concept needed; a bridge is a
  package on the topic protocol. This handoff makes the web UI itself behave
  like just another channel consumer.
- Downstream benefactor: the planned Telegram/Signal bridge — once worker DMs
  thread uniformly, "attach lanius to Signal so an agent can DM me"
  (`_questions.md`) is the same projection with a different egress package.
