---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-01
---

# Handoff: an agent that speaks on its own becomes a conversation you can reply to

Journey [../journeys/07-chatting.md](../journeys/07-chatting.md): "when the agent
acts on its own — kicked off by a GitHub issue, a timer, an inbound event — that
shows up as *another conversation you can step into and reply to*, not a
notification you can only watch." Today it does not. An agent that sends a message
without a preceding human prompt (a timer firing, an event handler) lands on the
owner's mailbox but produces **no conversation row**, so the person can see the
message go by but has nowhere to reply.

This is the known, recorded residual of
[chat-rendering.md](chat-rendering.md) (its M2 "Known limitation") and
[chat-conversations.md](chat-conversations.md) ("Ambient triggers are
conversations too"). Read both first — the model is settled; this handoff only
closes the one gap they named.

## The gap, exactly

The conversation projection is `conversation_rows` (`src/web.rs:2249`). It:

1. seeds a conversation row from every `in/agent/<agent>` inbound event
   (`src/web.rs:2261`), recording that event's session and mapping its
   correlation → session in `corr_to_session` (`src/web.rs:2272`);
2. folds the owner's `in/human/<owner>` messages in **only by correlation** — a
   human row is dropped unless its correlation is already in the map
   (`src/web.rs:2293-2295`, `else { continue; }`).

So a conversation only exists if it started with an `in/agent` prompt. A fully
unprompted `send_message` has no such prompt, its correlation is in no map, and no
row is ever created. `send_message` itself (`emit_message`, `src/exec.rs:865`)
emits to `in/human/<owner>` and does not stamp a session on the event
(`src/events.rs:116`, `session_id: None` by default).

## Wonky bits / decisions up front

1. **An ambient message must carry a session so it can anchor a thread.** The
   agent's run already has a session id. `emit_message` should stamp it on the
   event (the run's session), so the unprompted `in/human/<owner>` row has
   something to group by — exactly what `in/agent` events already provide. Confirm
   `EmitOpts` exposes the session field and that the run's session is reachable at
   the `emit_message` call site (it is emitted from within a run).

2. **Seed conversations from `in/human/<owner>` rows that carry a session, not
   only from `in/agent`.** This is the fix chat-rendering.md M2 spelled out: in
   `conversation_rows`, also select `in/human/<owner>` events, and for any that
   carry a session (and are not worker sessions), `ensure` a conversation the same
   way `in/agent` rows do. The correlation join for replies stays; this just adds
   a second seed source so an agent-first thread exists.

3. **Presentation only — no new authority.** This mints no tokens and gates
   nothing; it is a read/affordance change over the ledger, same stance as the two
   parent handoffs. A third-party UI must be able to reproduce it from the ledger.

4. **The label must be honest about who started it.** An ambient conversation was
   started by the agent, not "you". Its title is the agent's message preview and
   its source badge says the agent reached out (e.g. from a timer/event), not
   `you`. Reuse the existing `source_for` logic (`src/web.rs`) so the badge is
   derived, not hardcoded.

## Milestones

### M1 — An unprompted send carries its run's session
Stamp the run's session on the event `emit_message` publishes (`src/exec.rs:865`),
so an `in/human/<owner>` message sent with no prior human prompt still records a
session.

**Acceptance:** a unit test — emitting a `send_message` from a run with no
preceding `in/agent` prompt records an `in/human/<owner>` event whose session is
the run's session (not null). `ask_human` behavior is unchanged (it already
threads by correlation).

### M2 — The projection materializes ambient conversations
Extend `conversation_rows` (`src/web.rs:2249`) to also read `in/human/<owner>`
events and `ensure` a conversation for any that carry a (non-worker) session and
were not already seeded by an `in/agent` row. Keep the existing correlation join
for human replies to prompted threads.

**Acceptance:** with the ledger seeded with a single unprompted `send_message`
(session set, no prior `in/agent` prompt), `GET /api/conversations?agent=<name>`
returns one conversation for it. A prompted thread is still a single conversation
(no duplication when both an `in/agent` seed and correlated `in/human` replies
exist). `cargo test` green.

### M3 — It renders as a replyable thread with an honest source
The ambient conversation appears in the converse view as a labeled, replyable
thread; its title is the agent's message preview and its source badge marks it as
agent-initiated, not `you`.

**Acceptance:** `ui.spec.mjs` seeds an unprompted `send_message` conversation and
asserts it opens a `#view-converse` thread with at least one message and a source
badge distinguishing agent-initiated from human-initiated; replying into it
appends and threads by correlation with no duplicate (same reconciliation rule as
chat-conversations.md M4). Follows the `data-sel`/`waitForSelector` discipline;
rebuild + re-embed the SPA before running (web-embed staleness note in memory).

## Read these first
- The residual, named: [chat-rendering.md](chat-rendering.md) M2 "Known
  limitation"; [chat-conversations.md](chat-conversations.md) ("Ambient triggers
  are conversations too", M2/M4).
- The why: [../journeys/07-chatting.md](../journeys/07-chatting.md) ("What
  chatting should feel like").
- The code: `src/web.rs` (`conversation_rows` :2249, the `in/agent` seed :2261,
  the correlation-only human join :2286-2295, `source_for`, `session_for_event`,
  `is_worker_session` :2144); `src/exec.rs` (`emit_message` :865); `src/events.rs`
  (`EmitOpts`, `session_id` :116).

## Log
- 2026-07-01 — Created from the 2026-07-01 vision-drift recon. Re-verified the
  gap in the worktree: `conversation_rows` seeds only from `in/agent/<agent>`
  (`web.rs:2261`) and drops uncorrelated `in/human` rows (`web.rs:2293-2295`);
  `emit_message` (`exec.rs:865`) does not stamp a session, and events default
  `session_id: None` (`events.rs:116`) — so M1 (stamp the session) is a real
  prerequisite for M2, which the parent handoffs assumed. No authority change.- 2026-07-01 — All milestones implemented and adversarially verified (Opus
  impl/verify under Fable orchestration); landed on sprint-recon-2026-07.
  Status flipped to done.
