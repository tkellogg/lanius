---
status: planned
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-21
---

# Handoff: group resume-incarnations into one logical thread (read-model only)

A manual `elanus code <tool> --resume` mints a **fresh** elanus session id every
launch (`launch_session_id`), and the projection keys on `elanus_session`
(`code_session_stats` PK). So one continuous native coding thread (one Claude
`session_id` / one Codex `thread_id`) appears as **N separate sessions** — N roots
in the web tree, N rows in `elanus code sessions`, and a timeline shattered across
N `obs/agent/<noun>/<id>/…` subtrees. This collapses them into one logical
**thread** in the read model, so the listing and history reassemble by
`native_session`. **Display only — no change to identity, tokens, mailboxes, or the
spawn/resume machinery.**

## Why this is the right (and small) fix

From the `--resume` verification (the `_questions.md` "register the session
properly?" item): the resume path *works* — hooks fire on `--resume` because the
launcher always re-passes `--settings`, the durable record is written, every
incarnation is independently resumable. The **only** real impact is
**audit/history fragmentation**, which matters because audit is what elanus is for.
The functional paths are fine:

- The **daemon** resume (`resume_capture`, `codeagent.rs:2612`) **reuses** the
  recorded `elanus_session` and emits `session/resume` (not `session/start`) — so
  it already lands on the same row and bumps `resume_count`. **Not fragmented.**
- Only a **manual interactive `--resume`** goes through `launch()` → fresh id →
  `session/start` → a new `code_session_stats` row. **These are exactly the rows to
  regroup**, and they all carry the same `native_session`, so the key is already
  there.

So this is a query-time fold over data that already exists — not a schema or
identity change. Tokens (reaped), room/claims (reaped), per-turn injection
(recomputed) are ephemeral and unaffected; the narrow async-delivery-orphan edge
(a delivery addressed to a prior incarnation's id) is **out of scope** here (it's a
delivery-routing question, not a display one — see "Explicitly not in scope").

## The grouping key

```
thread_key(stat) = stat.native_session OR stat.elanus_session   // fallback when native is unknown
```

- `native_session` is the native thread identity (CC `session_id` / Codex
  `thread_id`) and is effectively globally unique, so it is a safe collapse key.
- **Fallback to `elanus_session`** when `native_session` is null — an incarnation
  that started but never produced a `cc_session`-bearing event (no tool calls /
  hook not yet linked) stays 1:1, which is correct (we can't claim it's the same
  thread). Robustness option: `LEFT JOIN` the authoritative durable mapping in
  `code_sessions` (codesession.rs) to fill `native_session` even when the
  obs-derived copy is null.
- Consider keying on `native_session` alone (not `(native, workdir)`): a native
  thread is tied to its workdir already; if incarnations ever disagree on workdir,
  surface the latest and note it rather than splitting.

## Where it goes

**In the projection read layer (`src/code_projection.rs`), not the TSX/JS** — so
the CLI (`elanus code sessions`/`<id>`, `main.rs:1003`) and the web UI
(`/api/code/sessions`, `server.mjs:971` → `CodeSessions.tsx`) both get it from one
implementation. `list_sessions` and `session_detail` are the two entry points.

## Milestones

### TG1 — Group the listing + union the detail timeline (the 80%)
- `list_sessions` returns one entry **per `thread_key`**, folding its incarnations:
  - representative/latest stat (tool, model, effort, workdir, last_status from the
    most-recent incarnation by `started_at`/`updated_at`);
  - `started_at` = min across incarnations, `last_active` = max;
  - `input_tokens`/`output_tokens` = sum; turn/event count = sum;
  - `incarnations: [elanus_session,…]` (the constituent ids, newest first);
  - **resumes**: report incarnation count and daemon-`resume_count` *separately* —
    `relaunches = incarnations-1`, `driven_resumes = Σ resume_count` — rather than
    one conflated number. (This is honest about the exact thing we're folding.)
- `session_detail(id)` accepts **either** an `elanus_session` **or** a `thread_key`
  and returns the **union timeline** across all incarnations of that thread, ordered
  by `ts` (then event id), with each event still labeled by its source incarnation.
  Resume command targets the **latest** incarnation / the native thread.
- **Acceptance:** after launching a CC session, killing it, and
  `elanus code claude --resume`-ing it twice, `elanus code sessions` shows **one**
  thread (not three) with `incarnations: 3`, and its detail shows the three turns'
  events in one time-ordered timeline.

### TG2 — Remap the spawn tree into thread-space
The web tree builds roots/children off the `parent` edge (an `elanus_session`).
After grouping, edges must connect **threads**: a thread's parent =
`thread_key(stat_of(parent_elanus_session))`. Roots = threads whose parent is unknown
or absent. This keeps the planner→worker spawn tree intact while collapsing
resume-incarnations (which are otherwise parentless roots).
- **Acceptance:** a planner that spawned a worker still shows worker-under-planner;
  three manual resumes of that worker show as **one** worker node, not three roots.

### TG3 — Render threads (web) + CLI ergonomics
- `CodeSessions.tsx`: render a **thread** row (rename `Stat`→`Thread` conceptually),
  expandable to its incarnations; the detail panel shows the unioned timeline and
  the `relaunches`/`driven_resumes` split. Minimal logic — the Rust layer already
  grouped.
- `elanus code sessions`: grouped by default; add `--raw`/`--ungrouped` to list the
  per-incarnation rows (debugging / the old behavior).
- **Acceptance:** the web session list shows one row per native thread; `--raw`
  still exposes every incarnation; nothing else in the view regresses.

## Explicitly not in scope (don't scope-creep into identity)

- **No identity change.** Each launch still mints its own elanus id and token; the
  `code_session_stats`/`code_sessions` rows stay 1:1 per incarnation. Grouping is a
  read-time fold, so every incarnation remains independently resumable and the
  daemon path is untouched.
- **No mailbox/delivery rerouting.** A delivery addressed to a prior incarnation's
  id staying in that id's mailbox is a separate concern (option (c) in the
  `--resume` discussion). If it ever bites in practice, that's its own handoff;
  this one does not move deliveries.
- **No bus/topic change.** Events keep filing under their incarnation's
  `obs/agent/<noun>/<elanus_session>/…`; the fold happens in the projection query,
  not the topic grammar.

## Read these first

- [coding-agent-observability.md](coding-agent-observability.md) — the projection +
  web session-tree this extends (the read model, `/api/code/sessions`,
  `CodeSessions.tsx`).
- [../../src/code_projection.rs](../../src/code_projection.rs) — `SessionStat`,
  `list_sessions` (479), `session_detail` (499), `code_session_stats` (PK
  `elanus_session`, has `native_session`), `STATS_COLUMNS` (425) — the layer to
  group in.
- [../../src/codesession.rs](../../src/codesession.rs) — the durable
  `code_sessions` mapping (`upsert_record`, keyed `ON CONFLICT(elanus_session)`),
  the authoritative elanus↔native source for the robust-key JOIN.
- [../../src/codeagent.rs](../../src/codeagent.rs) — `launch_session_id` (fresh id
  per launch, the fragmentation source) and `resume_capture` (2612, the daemon path
  that reuses the id and is *already* unified — the contrast that scopes this).
- [../../ui/web/src/CodeSessions.tsx](../../ui/web/src/CodeSessions.tsx),
  [../../ui/web/server.mjs](../../ui/web/server.mjs) (`/api/code/sessions`, 971).

## Log

- 2026-06-21 — Written after the `--resume` registration verification (empirically:
  hooks DO fire on `--resume` because the launcher re-passes `--settings`; the
  session registers and is resumable). The sole real impact is audit/history
  fragmentation from `launch_session_id` minting a fresh elanus id per manual
  resume, while the projection keys on `elanus_session`. Confirmed the fragmenting
  rows are exactly manual-relaunch incarnations (daemon `resume_capture` reuses the
  id and emits `session/resume`, so it never fragments) — so grouping by the
  already-present `native_session` cleanly reunites them as a read-time fold, no
  identity/mailbox change.
