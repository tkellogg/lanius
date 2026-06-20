# Handoff: conversations as first-class threads (the human's chat seat)

Make the web chat behave like the model already written in
[../journeys/07-chatting.md](../journeys/07-chatting.md): a human talks to an
agent in **conversations** (Slack-style threads), where one conversation is one
sliding-window context. A conversation is a first-class, **replyable** object
with a human label — not a raw kernel session id, and not a dead-end.

This is the chat counterpart to
[coding-agent-observability.md](coding-agent-observability.md). They meet at a
seam in the left nav: this handoff owns the **AGENTS / conversations** surface;
that one owns the **WORKERS / coding-runs** surface. The load-bearing decision
shared by both is that the UI must stop drawing every kernel `session` as an
identical pile of opaque hex.

## Read these first

- [../journeys/07-chatting.md](../journeys/07-chatting.md) — the journey; the
  "Current state vs. the gap" and "Design direction" sections are the spec for
  this work.
- [../layering.md](../layering.md) — the rule this handoff enforces: product UI
  must translate kernel vocabulary. "session" is kernel-speak; the product nouns
  are **conversation** and **run/worker**.
- [coding-agent-observability.md](coding-agent-observability.md) — the other half
  of the nav split; coding-tool agents (`claude-code`, `codex`) and their `code-*`
  runs leave the chat list and live in the Workers surface that handoff builds.
- [../../ui/web/src/App.tsx](../../ui/web/src/App.tsx) — the SPA. Anchors:
  `Nav` renders session ids as agent children (`:1071`); `submitCompose` caches
  one web session per agent per page load (`:880`); `onLiveMessage` mints a
  session per inbound obs/event (`:803`) and threads agent replies by correlation
  (`:810`–`:825`); `loadSessions`/`openTranscript` are the read-only SESSIONS tab
  (`:893`–`:924`).
- [../../ui/web/server.mjs](../../ui/web/server.mjs) — the relay; the new
  conversation-list/transcript endpoints land here, over `elanus.db`.
- [../bus.md](../bus.md) / [../topics.md](../topics.md) — `in/agent/<agent>` and
  `in/human/<…>` are the ledger-backed traffic a conversation is *made of*; this
  is why conversations can be projected from sqlite while worker obs cannot.

## The problem, named

Toured live on 2026-06-20 (`http://127.0.0.1:7180/`). Two complaints, one root
cause: the nav renders raw kernel session ids as first-class, same-indent
children under each agent (`App.tsx:1071`), and draws two different kinds of
object identically.

1. **Conversations are not replyable.** The converse feed threads correctly
   within a page load (one cached `web-main-*` session), but every reload mints a
   fresh session and every inbound event (`evt-web-*`) mints another. They pile up
   as bare ids, and clicking one routes to the **read-only SESSIONS transcript**,
   not the converse tab — so the thing that should be the first-class replyable
   object is shown as a non-interactive log line. This is "every message to `main`
   creates a new session you can't reply to."

2. **Coding runs dominate the sidebar.** `claude-code` and `codex` appear as peer
   chat agents, each with a stack of `code-*` run ids (up to 12 rows each) — no
   title, model, or status. These are dispatched worker processes, not
   conversations; they have a different lifecycle entirely (start/stop, exit code,
   tokens, parent→child tree). This is "the screen is full of random numbers I
   don't know what to do with."

## Design stance

- **Two product nouns, one kernel term.** A `session` surfaces as either a
  **conversation** (human ⇄ agent, replyable, one context) or a **run** (a coding
  process you observe). The UI never prints "session" in chrome; the raw id moves
  to a tooltip / detail line. (Enforces [../layering.md](../layering.md).)
- **A conversation is the unit of context.** One conversation = one
  sliding-window context = one session ([07-chatting.md](../journeys/07-chatting.md),
  "Never ending chat"). Continuity *across* conversations is carried by memory and
  the system prompt, not by merging threads.
- **Default to one never-ending thread, allow many.** Chatting with `main` from
  the web continues a single persistent "current conversation" across reloads;
  an explicit "+ new conversation" forks a fresh context; a "recent" list resumes
  any prior one. This is the doc's optional conversation-selector, done first as a
  manual control (an agent can drive it later — M5).
- **Ambient triggers are conversations too.** A GitHub/cron/event-originated run
  on `main` is *correctly* its own context, but it must appear as a labeled,
  **replyable** conversation so a human can jump into that thread — not a
  dead-end log entry.
- **Project conversations from the ledger; the UI is one client.** The
  conversation list and transcript come from `elanus.db` (the `in/#` traffic is
  sqlite-backed by construction), via server.mjs endpoints — so any other consumer
  reaches the same facts without scraping the DOM. (Same API-first stance as the
  observability handoff.)
- **Plumbing, no new authority.** This is a read/affordance change; it mints no
  tokens and gates nothing ([elanus-conventions]).

## Milestones

### M1 — Split the nav: conversational agents vs. coding workers

In `App.tsx` `Nav` (`:1045`–`:1083`):

- Stop rendering raw session ids as agent children (`:1071`). Under each *agent*,
  render **conversations** (M3) — labeled, clicking opens the **CONVERSE** tab
  bound to that session (M2), never the read-only SESSIONS tab.
- Remove coding-tool agents (`claude-code`, `codex`, any `elanus code` tool) from
  the **AGENTS** chat list. They render in a separate **WORKERS** section, owned
  by [coding-agent-observability.md](coding-agent-observability.md) and
  **collapsed by default** so a pile of `code-*` runs cannot dominate the 220px
  nav. Until that handoff lands, it is acceptable to list them collapsed with a
  count and a link to the SESSIONS/telemetry view — the contract here is only that
  they are *out of the chat flow*.
- Predicate for "is this a worker, not a conversational agent": prefer an explicit
  origin/kind on the record over id-prefix sniffing; `code-*` runs under a
  coding-tool agent noun is the acceptable v1 fallback.

**Acceptance:** the AGENTS list shows only conversational agents; each agent's
children are labeled conversations that open a replyable converse view; no bare
`code-*` id appears in the chat list.

### M2 — Conversation state: persist current, fork new, resume any

Frontend state (`App.tsx`, `agentSessions`/`submitCompose`/`selectAgent`):

- Persist the **current conversation** per agent (localStorage), keyed by agent,
  so a reload continues the same thread instead of minting a new `web-*` session
  (`:880`). New page load with an existing current → resume it.
- **"+ new conversation"** mints a new session, sets it current, clears the
  converse feed, focuses compose.
- Clicking a conversation (from the nav or a recent list) sets it current, loads
  its history (M4) into the converse feed, and **binds compose to that session**
  so the next message continues it. This is what makes event-originated threads
  replyable.

**Acceptance:** reloading the page keeps you in the same `main` conversation;
"+ new conversation" starts a clean context; clicking any conversation (including
a GitHub/cron-originated one) lets you reply into that exact thread.

### M3 — Conversation list API + recent list UI

Endpoint on [../../ui/web/server.mjs](../../ui/web/server.mjs), over `elanus.db`:

- `GET /api/conversations?agent=<name>` → `[{ session, agent, title, source,
  last_ts, message_count, preview, last_role }]`, newest first, **excluding worker
  runs**. Projected from `in/agent/<agent>` + `in/human/<…>` keyed by
  session/correlation.
  - **title**: first human prompt of the session, truncated (fallback: source +
    time). **source**: `you` / `web` / `github` / `cron` / … from the session
    origin. **preview/last_role**: the most recent message + who sent it.

UI: render this as the per-agent children in the nav (M1) and as a "recent"
section on the agent's landing, each row = title + relative time + a small source
badge — never the raw id (id → tooltip).

**Acceptance:** the nav/recent list shows human-readable conversation titles with
source and time; the raw session id is not the primary label anywhere in chat.

### M4 — Conversation transcript into the converse feed

Today converse holds only the in-memory live tail; durable history is stranded in
the read-only SESSIONS tab (`openTranscript`, `:904`). To resume a thread:

- `GET /api/conversations/:session` → the ordered messages (you / agent / ask /
  failed) for that session, so clicking a conversation rebuilds the full thread in
  **converse**, not just the live tail.
- Merge durable backfill with the live bus tail idempotently (key by
  correlation / message id) so a message present in both renders once — same
  reconciliation rule as the observability handoff's live ⊕ durable.

**Acceptance:** opening an older conversation shows its full history in a
replyable converse view; sending a reply appends and threads correctly with no
duplicate of the just-sent or just-received message.

### M5 — (Deferred, keep possible) the conversation selector as an agent

The doc floats an optional component that *chooses* which conversation to continue
or to start fresh, for near-optimal cache locality without losing the
never-ending-chat feel ([07-chatting.md](../journeys/07-chatting.md)). It must not
be core. M2–M4 are the hook: the same persist/fork/resume controls an agent would
drive. Do not build it; do not let the selection logic become UI-only — route it
through the M2 state and M3/M4 API so an agent can later make the same calls.

## Out of scope (recorded so it stays possible)

- **The worker card UI / subagent tree.** Owned by
  [coding-agent-observability.md](coding-agent-observability.md) (M1–M4 there).
  This handoff only *evicts* workers from the chat list and points at that
  surface.
- **HTML/form replies in-thread.** The doc's idea of an agent returning UI
  elements that continue a conversation without rebuilding context is a richer
  follow-on; the conversation model here is its prerequisite, not its delivery.
- **Parallel-session collision hints.** When two sessions touch the same
  resource, a human would want the UI to notice. Noted, not specced here.

## Log

- 2026-06-20 — Written from a live UI tour + an App.tsx data-model trace.
  Decisions from Tim: default to one persistent web conversation with an explicit
  "+ new conversation" and a recent list (the conversation-selector, manual
  first); ship as a plan/handoff (this doc), no code yet. Diagnosis and journey
  altitude live in [../journeys/07-chatting.md](../journeys/07-chatting.md); the
  worker half is [coding-agent-observability.md](coding-agent-observability.md).
