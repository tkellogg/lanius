# Handoff: coding-session observability in the web UI

Make a running coding session — and the tree of subagents it spawns — legible
from the **human's** seat: open the web UI and see the session you're driving,
a paste-able command to resume it, basic stats, and nested under it every
subagent it spawned with the same per-child card (which tool, model, effort,
duration, resumed?). Live, via the bus; durable, via sqlite.

This is the human-facing companion to
[coding-agent-dispatch.md](coding-agent-dispatch.md) (the agent-facing seam).
They meet at the record: the dispatch handoff's **D4b** captures the fields this
UI renders, so build them together.

## Read these first

- [../journeys/08-dispatching-a-worker.md](../journeys/08-dispatching-a-worker.md)
  — especially the *Tim's perspective* section, which is the spec for this work.
- [coding-agent-dispatch.md](coding-agent-dispatch.md) — D4b (capture
  model/effort/resumed/parent) is this handoff's hard prerequisite; the two
  dispatch modes explain why a live TUI session is the primary viewer.
- [coding-agents.md](coding-agents.md) — the envelope; obs grammar
  `obs/agent/<noun>/<session>/...` and the durable `code_sessions` record.
- [../bus.md](../bus.md) / [../../recorder.toml](../../recorder.toml) — the bus
  and the recorder's persistence rules (the obs-vs-ledger split below).
- [../../src/recorder.rs](../../src/recorder.rs),
  [../../src/codesession.rs](../../src/codesession.rs) (the `code_sessions`
  record + ledger queries), [../../ui/web/server.mjs](../../ui/web/server.mjs)
  (the web relay), [../../ui/web/src/App.tsx](../../ui/web/src/App.tsx) (the SPA;
  only a generic `obs/agent/<agent>/#` scope today — no coding-session surface).

## The architecture problem (why a new process)

Today there are two persistence paths, and coding-session telemetry is on the
wrong one for querying:

- **The ledger** (`elanus.db`, sqlite) is the `emit()` path: `in/#` and `signal/#`
  are sqlite-backed by construction, and the durable `code_sessions` record lives
  here. Queryable.
- **The recorder** sends obs to `trace.jsonl` — append-only, **write-only**,
  "nothing reads it for control flow" ([../../src/recorder.rs](../../src/recorder.rs)
  header). The coding session's real telemetry — `session/start` (tool, workdir,
  args), tool `call`/`result`, `session/idle` (token `usage`), `session/stop`
  (exit code) — lands here. **Not queryable.**

So the web UI cannot render a session tree with stats from what exists: the data
is either a write-only log (obs) or a per-session row with no rolled-up activity
(`code_sessions`). The missing piece is exactly what Tim intuited — **a process
that materializes the obs stream into queryable sqlite.** That is M1.

The web path is already shaped for the live half: the browser talks to
[../../ui/web/server.mjs](../../ui/web/server.mjs) (the relay run by `elanus serve`/
`dev`), which talks to the bus and the root/db. So "very connected to MQTT" =
server.mjs relays a live bus subscription to the browser; history comes from the
sqlite projection. Two feeds, one view: live deltas over the bus, durable
backfill + stats over sqlite.

## Design stance

- **Project a purpose-built table; don't dump raw obs.** The UI wants derived
  facts (duration, resume count, parent, latest status), so the materializer
  upserts a `code_sessions` projection (extend the existing record) + a compact
  `code_session_events` activity table — not a verbatim obs mirror. Keep
  `trace.jsonl` as the full flight recorder; the projection is the queryable
  index over it.
- **API first, UI second.** Every fact the UI shows must be reachable through a
  server.mjs endpoint over sqlite — because the (deferred) explainer agent and
  any other consumer must reach the same surface without scraping the DOM
  (dispatch handoff, Companion track). The UI is one client of the API, not the
  home of the data.
- **Plumbing + record, no new authority** (CLAUDE.md / [elanus-conventions]). The
  materializer is a kernel-side consumer of the bus, like the recorder; it mints
  no tokens and gates nothing. Reading the projection is a human/owner affordance.
- **Live ⊕ durable, reconciled.** The browser merges a sqlite backfill with a
  live bus tail; design for idempotent upserts keyed by session id + event so a
  replayed/at-least-once bus delivery and a backfill row converge, never double.

## Milestones

### M1 — The materializer: obs → sqlite projection

A bus consumer (a new module, or an extension of the recorder with a real
`ledger`/sqlite sink for `obs/agent/+/+/#`) that subscribes to coding-session obs
and upserts:

- **`code_sessions` (extend the existing record):** tool, agent noun, native
  session id, workdir, room, **model, effort** (from D4b), started_at, ended_at,
  exit_code, last_status, **parent_session** (from D4b), **resume_count**,
  rolled-up **token usage**/cost.
- **`code_session_events` (new):** a compact per-event row (session id, ts, kind
  — tool call/result, idle, resume, stop —, short summary, optional cost) so a
  session detail view can show a timeline without reading `trace.jsonl`.

Idempotent upserts keyed so a replayed bus event does not double-count (usage,
resume_count). Best-effort and crash-safe: a materializer that misses an event
must self-heal on the next (the bus is the source of truth; sqlite is the index).

**Acceptance:** with a live session running, the projection answers — from sqlite
alone — "list coding sessions with tool, model, effort, duration, token usage,
resume count, and parent" for the whole tree.

### M2 — The read API (server.mjs over sqlite)

Endpoints on [../../ui/web/server.mjs](../../ui/web/server.mjs), backed by the M1
projection:

- `GET /api/code/sessions` — the session list (roots + children), each with its
  card stats and `last_status`.
- `GET /api/code/sessions/:id` — one session's detail: full stats, the
  `code_session_events` timeline, and a **paste-able resume command** (derived
  from the native id + tool + workdir).
- `GET /api/code/sessions/:id/tree` — the nested subagent tree rooted at a
  session (parent edges from D4b).
- The full obs subtree for a session must be reachable here too (raw timeline) —
  this is the **data hook the deferred explainer agent will consume**; expose it
  now even though no explainer is built.

**Acceptance:** every field and relationship the UI renders is fetchable from an
API; nothing requires the browser to parse the bus or the trace file.

### M3 — Live feed (MQTT relay to the browser)

server.mjs relays a live bus subscription (`obs/agent/+/+/#`, scoped to coding
sessions) to the browser over a streaming transport (SSE/WebSocket — match
whatever App.tsx already uses for its obs scope). The browser merges live deltas
onto the M2 backfill, keyed idempotently so a session that started before the
page loaded (sqlite) and one that starts while watching (live) render the same.

**Acceptance:** open the UI while a session is mid-run and it appears and updates
without a reload; close and reopen and the full history is still there from
sqlite.

### M4 — The UI: session list → detail → subagent tree

In [../../ui/web/src/App.tsx](../../ui/web/src/App.tsx), a coding-sessions
surface (the SPA today has only a generic obs scope):

- A **list** of sessions (running first), each a card: tool, model + effort,
  duration, token usage, status, resume count.
- A **detail** view: the same stats, a copy-button **resume command**, and the
  event timeline.
- The **nested subagent tree**: a session expands to show the workers it spawned,
  each rendered with the same card recursively — "this Claude session spawned
  two codex workers; here's each one's model, effort, duration, and whether it
  was resumed."
- Live badges (running/idle/done) driven by M3.

**Acceptance:** Tim, in a TUI launched via `elanus code claude`, opens the web UI
and sees that session auto-appear with a resume command and stats, and — nested
under it — every subagent it spawned with the same information per child, updating
live.

## Out of scope (recorded so it stays possible)

- **The explainer agent.** A chat agent that narrates what a subagent did is
  deferred entirely (Tim, 2026-06-20). The only obligation here is M2's API
  exposing a session's full obs subtree, so the explainer can be built later
  against the same surface with no rework. Do not build it; do not let the data
  become UI-only.

## Log

- 2026-06-20 — Written from Tim's perspective added to
  [../journeys/08-dispatching-a-worker.md](../journeys/08-dispatching-a-worker.md).
  Grounding established before writing: obs persist to the **write-only**
  `trace.jsonl` (recorder), NOT sqlite; only `in/#`/`signal/#` and the
  `code_sessions` record are in the ledger — so a **materializer** (M1) is the
  load-bearing new piece, exactly Tim's "another process writing to sqlite."
  The web UI is fronted by `node ui/web/server.mjs` (the relay), so live = bus
  subscription relayed to the browser, durable = the sqlite projection. Decisions
  from Tim: live updates yes (very MQTT-connected); explainer agent deferred but
  must stay possible via APIs/data hooks (hence M2's raw-subtree endpoint and the
  API-first stance). Prerequisite: dispatch handoff **D4b** (capture
  model/effort/resumed/parent) — build the two handoffs' capture work together.
