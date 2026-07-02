---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: replying to a message branches a new conversation (Slack-style)

Tim's demo-day finding: you can only ever continue the *current* conversation.
There's no way to pick an old message — one from days ago, buried in a long
thread — and say "about *this*, …". Tim's decision: **clicking reply on any
message forks a brand-new conversation, seeded with the target message as
context.** The child thread shows what it branched from; the agent that receives
the branched prompt sees the quoted message (and its ledger id) so it knows
exactly what's being answered; the conversations list shows the branch
relationship.

The good news from grounding: the conversation projection already groups purely
by `session` + correlation, and the reply path is already a topic-generic publish
to `in/agent/<agent>` carrying `{prompt, session}`. A branch is "publish with a
*new* session id plus a structured `branched_from`" — no schema migration, no new
route. The work is making that branch **legible and reconstructable**.

## Wonky bits / decisions to confirm

1. **The branch link is a first-class field on the event payload, not just
   quoted prose.** Tim's ledger-honesty bar: "a third-party UI must be able to
   reconstruct the branch graph from the ledger alone." So the branching publish
   carries a structured `branched_from` object on the `in/agent/<agent>` payload:
   `{ event_id, corr, session, quote }` — the target message's ledger id (the
   anchor), its correlation and session, and its text. `event_id` is the edge a
   third party reads to draw child→parent. Seeding *only* a quote into the prompt
   text (no structured field) would read fine to a human but leaves the graph
   un-reconstructable — rejected. *Fable: confirm `branched_from` on the payload
   is the honest anchor, and that `event_id` (the ledgered target message row) is
   the right key to point at.*

2. **The kernel composes the agent-visible quote from `branched_from`; the UI
   sends only the structured field.** When a branched `in/agent` event is
   dispatched, the prompt the model sees should lead with the quoted target ("You
   were asked to respond to this earlier message: > …") followed by the person's
   new text. Doing that prepend **once, in the kernel** (the prompt-building site,
   `src/exec.rs:2874`) keeps the ledger clean (structured `branched_from`, not a
   pre-inlined blob) and means every branched run — from the web UI or any future
   client — gets the quote the same way. The UI just posts `{prompt, session,
   branched_from}`. *Fable: the alternative is the UI inlining the quote into
   `prompt` and the kernel staying dumb — simpler client-side, but then the quote
   and the structured field can drift, and a client that forgets to inline leaks a
   context-less prompt. I chose kernel-composes. Confirm.*

3. **The conversations list renders the branch as a flat row with a "branched
   from …" subtitle — not a nested tree.** The list today is flat, sorted by
   `last_ts`, keyed by `session` (`conversation_rows`, `src/web.rs:2430`;
   `App.tsx:2288-2303`). A true parent/child tree is more UI than this earns right
   now. Simplest honest rendering: the child conversation carries a
   `branched_from` summary (parent session + a short preview of the quoted
   message), and the list shows it as a subtitle/chip ("branched from: …") linking
   to the parent. **This is a wonky bit: the branch relationship is legible but
   *flat* — siblings that branched from the same parent aren't visually clustered,
   and a deep chain reads as a list of subtitles, not an indented tree.** State it
   plainly; a tree can come later if Tim wants it. *Fable: confirm flat-with-
   subtitle over nested-tree for this increment.*

4. **Presentation + a context seed — no new authority.** Same stance as
   [ambient-conversations.md](ambient-conversations.md): this mints no tokens and
   gates nothing. A branched prompt is an ordinary `in/agent/<agent>` publish (the
   person is already trusted to send those); `branched_from` is descriptive
   metadata the projection and the model read. The quote copies message text the
   person can already see. No confidentiality change.

**Product language.** "Reply", "branch/branched from", "conversation" are fine
person-facing words. Never surface "correlation", "session id", "event id", or
"payload" in the interface — the origin chip shows the *quoted text* and a human
timestamp, not raw ids ([../layering.md](../layering.md)).

## Milestones

### M1 — The branch publish + the kernel-composed quote
Client (`ui/web/src/App.tsx`): a reply on a message opens a **new** conversation
seeded with that message. Reuse `newConversation(agent)` (`App.tsx:666`, mints a
fresh `web-*` session, clears the feed) and extend `submitCompose`
(`App.tsx:1175`) so, when a branch target is pending, it publishes
`in/agent/<agent>` (`App.tsx:1191`) with `{ prompt, session, branched_from:{
event_id, corr, quote, session } }`. The target's `event_id`/`corr`/text are
already on the feed row (`m.id`, `m.corr`, `m.text`, `App.tsx:2307`).

Kernel (`src/exec.rs`): at the prompt-building site (`:2874`, where `payload.
prompt`/`payload.text` becomes the run's prompt), if the event payload has
`branched_from`, prepend the quoted target to the prompt the model sees, labeled
so the agent knows it's the message being replied to.

**Acceptance:** a unit test — dispatching an `in/agent/<agent>` event whose
payload carries `branched_from.quote = "…"` produces a run prompt that contains
both the quoted text and the person's new text, and the quote is attributed as
the branched-from message; an event with no `branched_from` is unchanged
(byte-identical prompt to today). `cargo test` green.

### M2 — The projection carries the branch edge (reconstructable from the ledger)
`conversation_rows` (`src/web.rs:2430`): when a session's seeding `in/agent` event
carries `branched_from`, expose it on the emitted row (`src/web.rs:2562-2593`,
alongside `title`/`source`/`preview`) as e.g. `branched_from: { session, event_id,
preview }` (a short preview computed from the quote). `conversation_messages`
(`src/web.rs:2596`): expose the branch on the thread so the feed can draw the
origin chip — either on the conversation object (`conversation` handler,
`src/web.rs:599`) or on the seed message. Keep it derived from the ledger event's
`branched_from`, never invented.

**Acceptance:** with the ledger seeded so conversation B's `in/agent` seed carries
`branched_from.event_id = <a message in conversation A>`, `GET /api/conversations
?agent=<name>` returns B with a `branched_from` referencing A (session + the
parent message's event id); `GET /api/conversations/<B>` exposes the branch origin
+ quoted text. A test asserts a third party could read `branched_from.event_id`
from the raw event payload and find the parent. `cargo test` green.

### M3 — The reply affordance + the origin chip + the list subtitle
- **Per-message reply affordance:** a reply control on each feed row
  (`App.tsx:2307`, the only per-message element — today it has no per-message
  button; a hover action is the natural fit) that starts a branch (M1).
- **Origin chip:** at the top of a branched thread, a quote/chip showing the
  target message's text and a link back to the parent conversation, rendered from
  the `branched_from` M2 exposes.
- **List legibility:** the conversations list (`App.tsx:2288-2303`,
  `.conv-recent-list`) shows a "branched from …" subtitle on child rows (wonky bit
  3, flat rendering).

**Acceptance:** `ui.spec.mjs` seeds a conversation with an old message, clicks its
reply affordance, and asserts: a new `#view-converse` thread opens; it shows an
origin chip quoting the target message and linking to the parent; the
conversations list shows the child with a "branched from" subtitle; sending in the
branch publishes an `in/agent` event carrying `branched_from` (assert via the
seeded ledger / a follow-up `GET /api/conversations`). Follows the `data-sel`/
`waitForSelector` discipline; rebuild + re-embed the SPA before running (web-embed
staleness note in memory).

## Read these first
- The model this extends: [ambient-conversations.md](ambient-conversations.md)
  (conversations are ledger-derived, presentation-only), [chat-conversations.md](
  chat-conversations.md) (the projection + correlation join).
- The why: [../journeys/07-chatting.md](../journeys/07-chatting.md).
- The projection: `src/web.rs` — `conversation_rows` `:2430` (row emit
  `:2562-2593`), `conversation_messages` `:2596` (`push_human_feed_message`
  `:2787`), the `conversations` handler `:578` and `conversation` handler `:599`,
  the topic-generic `publish` route `:354`.
- The client: `ui/web/src/App.tsx` — `ConverseView` `:2237`, the feed row `:2307`
  (carries `m.id`/`m.corr`/`m.text`), `submitCompose` `:1175` (`publish` at
  `:1191`), `newConversation` `:666`, `newWebConversationId` `:77`, the
  conversation list `:2288-2303`.
- The prompt build: `src/exec.rs:2874` (`payload.prompt`/`payload.text` → run
  prompt), the dispatch shape `:2853-2859`.
- The wording rule: [../layering.md](../layering.md).

## Log
- 2026-07-02 — Created from Tim's demo-day findings. Grounded against the
  worktree: the conversation projection groups purely by `session`+correlation
  (`web.rs:2430`/`:2596`), replies are a topic-generic publish to
  `in/agent/<agent>` with `{prompt, session}` (`App.tsx:1191`, `web.rs:354`), and
  a new `web-*` session already surfaces as a distinct conversation with no schema
  change — so a branch is "publish with a new session + a structured
  `branched_from`". Judgment calls for Fable: `branched_from` as a first-class
  ledger field for graph reconstruction (1); kernel composes the agent-visible
  quote so the ledger stays structured (2); flat "branched from" subtitle over a
  nested tree this increment (3).
- 2026-07-02 — All milestones implemented and adversarially verified (Opus
  impl/verify under Fable orchestration); landed on sprint-recon-2026-07.
  Status flipped to done.
