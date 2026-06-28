---
status: draft
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-28
---

# Handoff: sibling-intent — what each sibling is doing, and when it was last active

The work plan for [journeys/12-knowing-what-a-sibling-is-doing.md](../journeys/12-knowing-what-a-sibling-is-doing.md).
The rung past [sibling-awareness](sibling-awareness.md): SA1/SA2/SA4 + SA3's
write-half shipped, so a session is now *introduced* to its live siblings each
turn. This handoff makes the introduction carry **intent** (what each is working
on) and **recency** (when it was last active), and closes the **hookless-harness
gap** that makes codex/opencode siblings show up as bare name tags.

This is the runtime/kernel layer. Its agent-facing companion —
the skills that *use* this surface to resolve a conflict — is
[sibling-resolution-skills](sibling-resolution-skills.md). Build this first; the
skills degrade gracefully to today's CLI without it, but shine with it.

## The motivating incident (why this is real, not speculative)

A Claude session (me) was told "commit your work" and found eight modified files it
had never touched — a codex sibling's in-flight authority-delegation work
(`codesession.rs` +669) plus model-providers files plus `cargo fmt` churn. The
per-turn note told me three sessions were live and two were codex. It did **not**
tell me *which* was editing `codesession.rs*, *what* the change was, or *whether
that session was still typing or had wandered off*. I reconstructed all of it by
hand from `git diff` + recognizing `narrow_path_dim` by sight + a `git show` of the
merge author. Every step was archaeology over facts a sibling could have simply
advertised.

## Why it happened — the precise gap

SA2 already surfaces "last editing `<path>`" per sibling — but **only from
`code_claims`**, and claims are auto-recorded by the **SA3 write-half auto-claim
that fires on Claude's `Write`/`Edit` PostToolUse hook** (`codeagent.rs` ~2420). My
siblings were all **codex**, which runs with **no hooks** (`codex exec --json` is a
JSONL stream, no PreToolUse/PostToolUse) — so no auto-claim fired, so they carried
no claims, so the note showed name + tool and nothing else. Three concrete gaps:

1. **No recency.** `LiveSibling` (`codesession.rs:145`) is `{session, agent_noun}` —
   no timestamp. The column exists (`code_sessions.last_active`, `db.rs:341`) and the
   projection tracks `code_session_stats.updated_at` (most-recent obs event per
   session), but neither reaches the note.
2. **No intent.** The session's task list is observable —
   Claude's `TodoWrite` rides the `PostToolUse` hook
   (`obs/agent/claude-code/<s>/tool/TodoWrite/{call,result}`, payload carries the
   items), and codex already maps `todo_list` → `obs/agent/codex/<s>/assistant/todo`
   (`codeagent.rs` ~6060) — but nothing projects it into a queryable per-session task
   surface, so it never reaches the note.
3. **Hookless harnesses are mute.** codex/opencode produce no auto-claims, so "what
   each is touching" is empty for exactly the harnesses I collided with. The signal
   exists elsewhere — the fs **write camera** publishes `obs/fs/<path>` carrying the
   acting `session_id` in the trace envelope (`exec.rs:2020`), and the stream
   mapping already emits `tool/<name>/call` for codex/opencode edits — but no
   consumer turns either into a claim.

## Milestones

### SI1 — Last-active in the sibling note (smallest, highest-value)
Surface recency so a session can judge *alive vs stranded* — the one fact that
decides whether a sibling's WIP is safe to touch.
- Enrich `LiveSibling` (`codesession.rs:145`) with `last_active: String` (RFC3339).
  Populate it in `live_siblings` (`codesession.rs:158`) as the **fresher of**
  `code_sessions.last_active` and `code_session_stats.updated_at` (the projection's
  per-session most-recent-event time) — the LEFT JOIN is already in that query's
  neighborhood.
- Keep `last_active` genuinely live: bump `code_sessions.last_active` from the obs
  publish path (or once per turn from the hook/stream capture), not only on
  resume/`upsert_record` (`codesession.rs:107`) as today — otherwise a long-running
  session reads as stale.
- Render it in the note (`turn_injection`, `codeagent.rs` ~3013): `code-3361aa9d
  (codex, last active 30s ago)`. Use a humanized delta ("30s ago" / "18m ago"); the
  `LIVE_WINDOW_SECS=900` filter already bounds it to ≤15m, so anything near the edge
  legitimately reads "stranded — likely safe."
- **Acceptance:** two sessions in one workdir; the viewer's `[elanus siblings]` line
  shows each sibling's `last active <delta>`; a session idle >`LIVE_WINDOW` drops off
  the roster entirely (existing behavior, unchanged).

### SI2 — Intent broadcast: each session's task list as ambient state
The heart of journey 12 — turn the name tag into "what it's working on."
- **Capture (no new harness wiring needed for 2 of 3):**
  - *Claude:* the `TodoWrite` `PostToolUse` event already lands on the bus; project
    its `tool_input` (the items + statuses) instead of letting it fall on the floor.
  - *codex:* `assistant/todo` already carries `items` — project it.
  - *opencode:* **gap** — `opencode run --format json` emits no todo/plan event
    (`opencode_map_event`, `codeagent.rs` ~5523, has no todo arm). Degrade honestly:
    an opencode sibling shows touches (SI3) but no task list, and the note says so
    rather than implying it has none. (A future opencode that emits a plan event
    plugs in here.)
- **Project:** a new `code_session_tasks` table (mirror `code_session_stats`'s
  trace-fold pattern in `code_projection.rs`): `(elanus_session, item_id, text,
  status, updated_at)`, folded from the todo obs events, latest-wins per item. One
  row per task item; status ∈ `todo|in_progress|done`.
- **Surface:** the sibling note gains the current `in_progress` item (+ a
  `todo/done` count) per sibling — `code-3361aa9d (codex, last active 30s ago):
  in_progress "authority-delegation: child-grant narrowing"`. Also expose it on
  `elanus code sessions` / `elanus code rooms` (the human + the skills read it there).
- **Acceptance:** a sibling running a multi-item task list shows its current
  `in_progress` item in the viewer's note and in `elanus code sessions`; as the
  sibling advances items, the surface refreshes on the next projection tick.

### SI3 — Touch-derived claims for hookless harnesses (close the codex/opencode gap)
Make "what each is touching" work for codex/opencode, which produce no auto-claims —
the exact gap that left my codex siblings mute. This is the deferred **SA3
auto-claim consumer**, now scoped concretely.
- Build a consumer that tails the bus and converts touches into advisory claims via
  the existing `add_claim` (`codesession.rs:679`):
  - Primary source: the fs **write camera** `obs/fs/<path>` events, which carry the
    acting `session_id` in the trace envelope (`exec.rs:2020`) — harness-agnostic, so
    it covers codex/opencode/claude uniformly.
  - The consumer maps `session_id` → its room (`session_auto_claim_room_and_workdir`,
    already used by the Write/Edit auto-claim) and `add_claim(room, session, path)`.
  - Claims should expire/refresh so a touch from 20m ago doesn't read as current
    (tie to `last_active` / a TTL on auto-claims).
- This unifies the SA3 write-half across all three harnesses (today only Claude's
  hook path produces auto-claims) and removes the reason my codex siblings showed no
  "last editing".
- **Acceptance:** a codex sibling that writes `src/foo.rs` shows `last editing
  src/foo.rs` in the viewer's note **with no hook and no manual `claim`** — purely
  from the fs camera. Parity with the Claude auto-claim path.
- **Note:** the read-half (attributing *reads*) stays deferred behind the read
  camera (`sandbox.md` "[OPEN]"); this milestone is writes only.

### SI4 — Change attribution backend (`whose-change`)
Answer "which of these dirty files are mine, and who owns the rest?" — the question
my human and I both got wrong about the codex WIP.
- A query/CLI that maps a path (or the working tree's `git status` set) → owning
  session, from two sources, freshest-wins:
  - `code_claims` (auto + manual claims, now covering all harnesses after SI3), and
  - `obs/fs/<path>` events' `session_id` (direct write attribution).
- Expose as `elanus code whose <path>` (and a bulk `elanus code whose --dirty` that
  takes `git status --porcelain` and annotates each file with its owner + that
  session's last-active + current task). Back it the same way `elanus code rooms`
  is backed (a projection/ledger read).
- **Acceptance:** with a sibling's uncommitted change present, `elanus code whose
  <file>` names the owning session, its tool, last-active, and current task;
  `--dirty` annotates the whole `git status` set. Files only the viewer touched
  attribute to the viewer.

## Data model summary (new)
- `code_session_tasks(elanus_session, item_id, text, status, updated_at)` — SI2
  projection of todo obs events; latest-wins per `(session, item_id)`.
- `LiveSibling` gains `last_active` (and optionally `current_task`) — SI1/SI2.
- Auto-claim TTL/refresh on `code_claims` — SI3 (so touches age out).
- No new transport: everything rides the existing bus topics and the
  `code_projection` trace-fold.

## Verification
- `cargo test` green; unit tests per milestone (last-active fresher-of-two;
  task-list projection latest-wins per item; obs/fs → claim mapping carries the
  right session; `whose` resolves a path to an owner).
- End-to-end (mirrors the incident): launch a codex worker that edits a file and
  sets a todo list; from a second session assert the note shows that sibling's
  `last active <delta>` + `in_progress` task + `last editing <path>`, and
  `elanus code whose <file>` names the codex session. This is the exact scenario
  that was invisible to me.
- Honesty checks: opencode sibling shows touches but a clearly-absent task list (not
  a fake empty one); a stranded session reads as stale, not current.

## Anchors
- `src/codeagent.rs` — `turn_injection` (~2904, sibling line ~3013), `claude_map_event`
  TodoWrite (~6755), codex `todo_list`→`assistant/todo` (~6060), auto-claim on
  Write/Edit (~2420), `session_auto_claim_room_and_workdir`.
- `src/codesession.rs` — `LiveSibling` (145), `live_siblings` (158), `peer_claims`
  (720), `add_claim` (679), `upsert_record`/`touch_record` (107/877).
- `src/code_projection.rs` — `code_session_stats` + `updated_at` fold (the model to
  copy for `code_session_tasks`).
- `src/exec.rs` — fs write camera + `ids.session_id` (~2020); `src/trace.rs` `Ids` (8).
- `src/db.rs` — `code_sessions` (341), `code_claims` (457), `code_room_members` (439).

## Residuals / out of scope
- **opencode task list** — no plan/todo event in its stream today; SI2 degrades it to
  touches-only and says so. Revisit when opencode emits one.
- **Read attribution** — `whose-change` covers writes (fs write camera); read
  provenance waits on the read camera (`sandbox.md`).
- **UI** — surfacing intent/last-active in the web rooms/sessions views is a natural
  follow-on, not in this handoff.
