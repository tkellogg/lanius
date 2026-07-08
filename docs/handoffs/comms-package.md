---
status: planned
author: Opus 4.8 in Claude Code (planner)
last-updated: 2026-07-08
---
# Handoff D: make comms a package — the chat concern owns its own protocol, history, and introspection

Tim's intent, verbatim: *"If we're making comms a package, we should have as part
of that package a process that serializes chats to disk. So one package gives you
(1) the chat protocol on MQTT and (2) a history API and an introspection API."*

So: **one package that owns the conversation/chat concern end to end** — the chat
topic conventions on the bus, a process that turns the raw event stream into
chat-shaped conversations on disk, and a history + introspection API over them.
This is the "simple core + packages" design law (a capability is a package, not a
kernel-baked special case) applied to chat and history.

**The good news you should read before anything else: half of this already
exists, and it proves the design.** The `history` package
(`kits/stdlib/packages/history/`) is a userland daemon that answers read-only
history queries — `agents`, `sessions`, `transcript`, `conversation`, `search` —
over its own loopback HTTP port, reading `lanius.db` strictly read-only ("NO state
of its own — every query is answered by a fresh read-only sqlite connection"). The
web server already **relays** to it: `/api/history` proxies to
`run/pkg-history/http.json` and degrades to live-only when the package is parked
(`src/web.rs:1013-1058`, `history_endpoint` `:2007`). That is exactly the shape
this handoff wants, already shipped for the transcript/search side.

**What is still kernel-baked, and is the real work here:** the *chat-shaped
conversation view* — threading events into conversations, labeling each
conversation's source/channel, deciding which sessions are worker runs vs. human
comms — lives hard-coded in the core web server: `conversation_rows` /
`source_for` / `is_worker_session` (`src/web.rs:2361-2668`) behind
`/api/conversations`. Adding a channel or changing the chat projection today means
editing the kernel. This handoff **moves that projection into the comms package**
and turns `/api/conversations` into a relay, exactly like `/api/history` already
is — de-kernelizing precisely the hard-codes the recent audit flagged
(`docs/channels.md` closing section: *"`web.rs` `is_worker_session`/`source_for`
hard-code the channel taxonomy into the core conversation projection — adding a
channel edits the kernel"*).

## The key design fork — resolve this first

Tim said "a process that **serializes chats to disk**." There are two readings,
and they are not close.

- **(a) Additive projection — RECOMMENDED.** The kernel keeps owning durable
  message persistence (`in/#` and `signal/#` materialize to the ledger —
  `src/broker.rs:970-998`; `docs/topics.md`: *"the recipient's mailbox is the
  single durable copy"*). The comms package is a **read view on top of that**: it
  reconstructs the chat-shaped conversation projection from the ledger and serves
  it, plus history and introspection APIs. The conversation-projection logic now
  in `src/web.rs` **moves into the package**; the web UI becomes a thin relay of
  the package's API (the `/api/history` pattern, applied to `/api/conversations`).

- **(b) Move durable persistence out of the kernel.** The broker stops
  materializing `in/#`; the comms package owns the durable write path. This
  **contradicts** a load-bearing kernel guarantee ("the mailbox is the single
  durable copy," `docs/topics.md`) and the whole no-`out/` decision that keeps
  exactly one durable copy per message. It is a large, risky core change and buys
  a consistency obligation the architecture deliberately refuses.

**Recommend (a), strongly — and the `history` package is the living proof it is
the right call.** `docs/bus.md` already decided this exact point:
*"reconstruction views are USERLAND subscribers, never kernel."* The `history`
package is the first such view and holds no state of its own. The comms package is
the second: the same read-view pattern, grown to cover the chat-shaped projection
the kernel hasn't yet handed off. (a) is not a compromise — it is the pattern the
codebase already commits to.

### Sub-fork inside (a): does the comms package keep its OWN sqlite?

Tim's words — "serializes chats to disk" — sound like the package holds a store,
the way `packages/phonebook` and `packages/recall` each own a sqlite in their
scratch. Be honest about what "to disk" already means here: **the kernel already
serializes every chat message to disk** — that is what `in/# → ledger` is. So the
package does **not** need a second durable copy of the messages, and it should not
have one (a second write path is the consistency debt (b) was rejected for).

Two ways to shape the package's store, in order of preference:

- **(a1) No own store — reconstruct on read (RECOMMENDED to start).** Mirror the
  `history` package exactly: every query opens a fresh read-only connection to
  `lanius.db` and computes the conversation projection on the fly, the way
  `conversation_rows` does today. Simplest, zero consistency debt, and the
  projection is always current with the ledger. Tim's "serializes chats to disk"
  is satisfied by the kernel's ledger; the package supplies the *chat shape and
  the API*, which is the part that's actually missing.

- **(a2) A derived, rebuildable index — only if introspection needs it later.** If
  conversation-list or introspection queries get too heavy to recompute per
  request over a large ledger, the package MAY maintain its own sqlite as a
  **materialized projection** — a cache it builds by subscribing to the chat
  topics, explicitly **not a second source of truth**, droppable and rebuildable
  from the ledger at any time (the way a search index relates to its corpus). This
  is the phonebook/recall "own sqlite" shape, but for *derived* data, not
  authoritative data. Defer it; don't build it speculatively.

**Recommendation: (a) + (a1).** The kernel keeps durability; the package owns the
chat projection + the history/introspection APIs, reconstructing on read. Leave
(a2) as a documented optimization behind a real performance finding.

## Decisions to confirm / wonky bits

1. **Standalone `comms` package that requires `history` (Tim's decision).** The
   conversation-list projection is a reconstruction view over the same read-only
   `lanius.db` that `history` already serves. Rather than merge them, **comms is its
   OWN package** — `packages/comms`, cloning the `history` daemon shape (a read-only
   sqlite view served on a loopback port) — that **declares `[requires] packages =
   ["history"]`** using Handoff E's dependency mechanism. This keeps the chat
   capability *independently approvable* (an agent can hold `comms` without full
   transcript search, or vice versa) and **composes the two packages via a
   dependency edge rather than merging them** — the package-composition principle
   Tim set for this whole sprint. The cost (a second daemon/port/proxy reading the
   same db) is accepted; the win is granular capabilities and a clean, declared,
   E-validated dependency. **Ordering: build Handoff E BEFORE this handoff**, so the
   `requires = ["history"]` declaration is real and validated the moment comms lands.
   The milestones below say "the comms view" — read that as the new `packages/comms`
   daemon.

2. **The "history API" half is already built.** `history`'s `transcript`,
   `conversation`, and `search` kinds ARE the history API Tim named
   (`kits/stdlib/packages/history/SKILL.md`). Don't rebuild it. This handoff adds
   the *chat-shaped conversation list* (missing) and an *introspection* surface
   (new), and moves the projection out of the kernel. Confirm we treat the
   existing history API as done and build only the delta.

3. **Byte-compatibility during the move is non-negotiable.** `/api/conversations`
   has live consumers (the web comms list, e2e `ui.spec.mjs`). The package must
   serve the **same JSON shape** `conversation_rows` returns today
   (`src/web.rs:2665+`), so the web server can relay without the SPA noticing. The
   move is a relocation, not a redesign of the payload. Confirm: the port is
   behavior-preserving; any shape change is a separate, later handoff.

4. **What moves, and what the legacy fallbacks do.** `source_for`
   (`src/web.rs:2385`) already prefers an explicit `payload.source` before falling
   back to spelling-based guesses (`web-`, github/jira/linear, cron/timer). Those
   fallbacks **travel with the projection into the package** — they don't get
   deleted here (github/jira/linear may still have no package stamping their
   source). They become a shrinking safety net *inside the package*, with a TODO,
   not a kernel special-case. Confirm we relocate the fallbacks rather than block
   the move on emptying them. (This is the seam Handoff C's M3 feeds — see below.)

5. **De-magic depends on C's source stamp, not on A.** Once the source is stamped
   at the source (Handoff C M3 makes send_message/ask_human/bridge paths stamp
   `payload.source`), the package's `source_for` resolves new channels via data,
   and the legacy fallbacks wither. `is_worker_session` (`:2381`) keys on the
   `code-*` session id and is independent of Handoff A's stored `kind`; if A lands,
   fold it onto A's `kind` helper, otherwise leave it as-is (same note C M3 makes).
   Confirm: this handoff does not itself depend on A.

## Milestones

### M1 — move the conversation projection into the comms view (behavior-preserving)

Port `conversation_rows` + `source_for` + `is_worker_session` + the threading
helpers (`session_for_event` `:2361`, `Convs`/`touch`/`ensure`, `fold_human_payload`,
`branch_row_summary`, the ambient-seed and correlation-join passes,
`src/web.rs:2361-2668`) into the new `packages/comms` daemon (cloning the `history`
daemon shape, and declaring `[requires] packages = ["history"]`) as its
`conversations` query kind — args `{agent}` — reading `lanius.db` read-only exactly
as the `history` kinds do. Carry
the source-labeling fallbacks verbatim (decision 4) with a TODO pointing at
"channel/package-declared source." Do NOT yet touch `/api/conversations` — the
in-core path still serves; this milestone only makes the package able to produce
the identical result.

- **Acceptance:** a golden test — for a seeded ledger, the comms view's
  `conversations` query returns JSON **byte-equal** (modulo ordering guarantees) to
  what `conversation_rows` returns in-core for the same `agent` and owner. Covers
  the tricky cases the in-core tests already exercise (`web.rs:3162,3270`): worker
  sessions evicted, ambient/agent-first conversations seeded, correlation-joined
  human replies folded, `branched_from` surfaced, `source` labeled (you/web/etc.).

### M2 — relay `/api/conversations`, retire the kernel projection

Make `conversations` (`src/web.rs:624`) proxy to the comms view — the same shape as
`history` (`:1016`): read the endpoint from the run dir per request
(`history_endpoint`-style, healing across restarts), POST the `{kind:"conversations",
agent}` query, pass the body through, and **degrade gracefully** when the package
is parked/unreachable (a clear "comms view unavailable — approve the package"
message, mirroring `:1042-1047`, not a 500). Then **delete** `conversation_rows` /
`source_for` / `is_worker_session` and their helpers from `src/web.rs` (or keep a
thin, clearly-labeled in-core fallback only if the team wants the web comms list to
survive the package being parked — recommend NOT: match `/api/history`'s
"degrade to unavailable" posture so there is exactly one projection, in the
package). Update `/status` (`:494`) to report the comms view's availability like it
reports history's.

- **Acceptance:** with the comms package approved and running, the web comms list
  is identical to before the change (drive `ui.spec.mjs` with node OFF PATH against
  the Rust server — remember the embed-staleness gotcha: `npm run build` → `touch
  src/web.rs && cargo build` → run e2e). With the package parked, `/api/conversations`
  returns the graceful-unavailable JSON and the SPA degrades, not a crash. Grep
  confirms the channel-taxonomy hard-codes (`web-`, github/jira/linear, cron) no
  longer appear in `src/web.rs` — they live in the package now.

### M3 — the chat-protocol SKILL: the package documents the conventions it owns

Add/extend the package's `SKILL.md` to state the **chat protocol on the bus** — the
"(1)" half of Tim's ask — in plain language, naming the real topics: the
conversation locator rides the topic (`in/agent/<noun>/<conv>`, `docs/topics.md`
"Conversations get their own identity"); external conversations are addressed by
`in/dm/<kind>/<addr>` (Handoff B's grammar); a conversation is threaded by its
locator, not a room; `source` is a stamped fact (`payload.source`), not spelling;
receipts land on `obs/channel/<kind>/{sent,acked}`. This is the read-side companion
to `docs/channels.md`'s "a transport is just a package": the comms package is where
"what a conversation is, on the wire" is written down and where it is served back.

- **Acceptance:** the package's SKILL names the real conversation/`dm` topics and
  the source-stamp convention, and points at `docs/channels.md` + Handoff B for the
  grammar. A reader can learn the chat protocol and the query API from one package.
  (Content milestone — acceptance is that it's accurate and cross-referenced, not a
  code test.)

### M4 — the introspection API (the "(2b)" half Tim named)

`history`'s existing kinds answer "replay this session / search the corpus." The
**introspection** Tim named is the conversation-level question the comms package is
uniquely placed to answer: *for a conversation, who is in it, on which channels,
and how is it composed?* Add an introspection query (e.g. `kind:"conversation_info"`,
args `{session|conv}`) that returns, from the ledger (+ the phonebook's HTTP read
plane where identity resolution helps): participant/sender set, source/channel,
message + turn counts, first/last timestamps, branch origin, and the correlation(s)
threaded into it. Keep it a **read** — no writes, provenance from the
broker-verified `sender` on each event, never a payload field (the same trust rule
recall and the phonebook already hold).

- **Acceptance:** for a seeded conversation, `conversation_info` returns its
  participants (broker-verified senders, not payload-claimed), its source, its
  message/turn counts and time span, and its branch origin if any. A conversation
  spanning a `dm` channel reports that channel; a purely-internal one reports its
  internal source. Assert the participant set comes from verified `sender`, not a
  body field (a forged `payload.sender` does not appear).

## Read these first

- `kits/stdlib/packages/history/` — **the template that already exists**: the
  manifest (`process.http`, daemon, read-only), `SKILL.md` (the query DSL:
  `agents`/`sessions`/`transcript`/`conversation`/`search`), and `scripts/main`
  (fresh read-only sqlite per query, no own state). The comms view is this, grown.
- `src/web.rs:2361-2668` — the projection to move: `session_for_event`,
  `is_worker_session`, `source_for`, `Convs`, `conversation_rows`, the ambient-seed
  and correlation-join passes, `branch_row_summary`. And `:3162,3270` — the
  existing projection tests to preserve as the golden.
- `src/web.rs:1013-1058` + `:2007` — `/api/history` and `history_endpoint`: the
  relay + graceful-degradation pattern M2 copies for `/api/conversations`.
- `src/web.rs:624-643` — `conversations` (the route that becomes a relay) and
  `:471,:494` (`/status`'s history-availability line to mirror).
- `src/broker.rs:970-998` — where `in/#`/`signal/#` materialize to the ledger (the
  kernel durability guarantee (a) preserves and (b) would break).
- `docs/channels.md` — closing section names these exact hard-codes as audit debt;
  the whole "a transport is just a package" case is the design this completes on
  the read side.
- `docs/topics.md` — "the recipient's mailbox is the single durable copy"; the
  conversation-locator decision ("Conversations get their own identity").
- `docs/bus.md` — "reconstruction views are USERLAND subscribers, never kernel"
  (the decision that makes (a) the settled pattern, not a judgment call).
- `packages/phonebook/` + `packages/recall/` — the package-with-own-sqlite + read
  API shape (relevant only for the deferred (a2) sub-option; recall is also the
  identity-resolution the M4 introspection can lean on).
- Handoffs **B** (`dm-channel-grammar.md`) — the `dm`/conversation grammar this
  package's protocol SKILL builds on — and **C** (`agent-dm-relay.md`), M3 — the
  minimal source-stamp seam this handoff's fuller move consumes (boundary below).

## The D-vs-C boundary (read this so the two handoffs don't collide)

Handoff **C** (`agent-dm-relay.md`) **M3** does a *minimal de-magic in place*: it
makes the send_message/ask_human/bridge paths **stamp `payload.source`** so
`source_for`'s existing `payload.source` branch (`web.rs:2386`) resolves new
channels, and it leaves the projection where it is, in the kernel, with the legacy
fallbacks as a shrinking net. C changes the *data going in*; it does not move the
*reader*.

Handoff **D** (this one) does the *fuller move*: it relocates the whole
conversation projection — `source_for` and all — **out of the kernel into the comms
package**, and turns `/api/conversations` into a relay.

**D builds on C; it does not subsume it, and it does not redo it.** C's
`payload.source` stamp is the data seam D's relocated `source_for` reads; the
legacy fallbacks C shrinks are the ones D carries into the package (decision 4).
So: **sequence D after C.** If C lands first, D inherits a projection that already
prefers stamped source and simply relocates it. If for some reason D were done
first, D would relocate `source_for` *with its current fallbacks*, and C's stamp
would then land on the package's copy instead of the kernel's — workable but
mildly more churn. Prefer C then D. D must **not** re-implement C's stamping (that
is C's job on the emit paths); D only moves the reader that consumes it.

## Residuals / gating

- **The history API is already built** (`kits/stdlib/packages/history`) — this
  handoff adds the conversation-list + introspection delta and moves the projection
  out of the kernel; it does not rebuild transcript/search.
- **Own-store (a2) is deferred** behind a real performance finding — start with the
  reconstruct-on-read (a1) shape the `history` package already uses. Don't build a
  materialized index speculatively; if built later, it is a rebuildable derived
  cache, never a second durable copy.
- **The legacy source fallbacks** (github/jira/linear/cron) move into the package
  as a shrinking net with a TODO; they empty only as those sources become packages
  that stamp their own `payload.source` (channels.md closing section). Not this
  handoff's job to delete them.
- **Depends on Handoff C** (the source stamp + the shrunk fallback net make the
  relocated `source_for` clean). **Independent of Handoff A** (`is_worker_session`
  keys on the `code-*` session id; fold onto A's `kind` helper only if A has
  landed). **Soft-relates to Handoff B**: the protocol SKILL (M3) documents B's
  `dm` grammar, but the projection move (M1/M2) doesn't require B.
- **The embed-staleness gotcha bites M2's verification**: `elanus web` embeds
  `ui/web/dist` at compile time and `npm run test:ui` never cargo-builds, so run
  `npm run build` → `touch src/web.rs && cargo build` → e2e, or you may verify a
  stale SPA against the new relay.
- **Byte-compatibility is the safety rail** (decision 3): the package serves the
  same `/api/conversations` shape today; any payload redesign is a later, separate
  handoff so the web UI and e2e keep working through the transition.

## Log

- 2026-07-08 — planner drafted from the worktree. **Key finding:** the "history +
  introspection API over a userland read view" pattern Tim wants **already exists**
  as `kits/stdlib/packages/history` (daemon, own HTTP port, read-only `lanius.db`,
  no own state; `/api/history` already relays to it with graceful degradation). The
  only chat-shaped view still kernel-baked is the conversation-list projection
  (`conversation_rows`/`source_for`/`is_worker_session`, `src/web.rs:2361-2668`
  behind `/api/conversations`). So the comms package = grow that read view to own
  the conversation projection + a conversation-introspection surface + the
  chat-protocol SKILL, and turn `/api/conversations` into a relay like
  `/api/history`. Recommended the design fork as **(a) additive projection** — the
  kernel keeps `in/# → ledger` durability ("the mailbox is the single durable
  copy"), the package owns the projection + APIs — grounded in `docs/bus.md`'s
  decided "reconstruction views are USERLAND, never kernel." Within (a),
  recommended **(a1) reconstruct-on-read** (no second durable store; the kernel
  already serializes chats to disk), with (a2) a rebuildable derived index deferred
  behind a performance finding. Recommended **growing the `history` package** (add a
  `conversations` kind) over a standalone `comms` package, unless chat wants
  independent approval. Set the **D-after-C** boundary: C M3 stamps `payload.source`
  in place; D relocates the whole projection (incl. `source_for` + its fallbacks)
  out of the kernel and consumes C's stamp — D builds on C, does not redo it.
- 2026-07-08 (Tim's decision, via tech-lead) — **standalone `comms` package, NOT
  growing `history`.** Overrode the planner's grow-history rec: comms is its own
  `packages/comms` that `requires = ["history"]` (Handoff E's dependency mechanism),
  keeping the chat capability independently approvable and composing packages via a
  declared dependency edge rather than merging them (Tim's package-composition
  principle). New ordering constraint: **build E before D** so the `requires` is
  E-validated on landing. Milestones' "comms view" = the new `packages/comms` daemon.
</content>
</invoke>
