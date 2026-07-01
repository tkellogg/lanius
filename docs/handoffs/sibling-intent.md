---
status: done
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
3. **Hookless harnesses appeared mute** — but the mechanism already exists.
   _(Correction, post-implementation:)_ codex/opencode DO auto-claim, from each
   harness's OWN write-tool events: `auto_claim_write` is wired at the codex
   `file_change` capture (`codeagent.rs:6069`) and the opencode `edit`/`write`
   capture (`codeagent.rs:5709`), with tests. This is a **settled** design decision
   (`codeagent.rs:2731`): the fs `obs/fs` write camera only brackets *caged* actors
   (the kernel shell/package actors), and coding harnesses run their OWN sandboxes
   *outside* elanus's cage — so an `obs/fs` subscriber would NEVER witness a coding
   agent's edit. So my codex siblings showing no "last editing" was **not** a missing
   mechanism; it's operational (their `file_change` summaries arrive at turn end, and
   the human's siblings may not have been captured by elanus at all). SI3 below is
   therefore "verify the existing path," not "build a new one."

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

### SI3 — Touch-derived claims for hookless harnesses — ALREADY SHIPPED (verify only)
**Do NOT build an `obs/fs` consumer.** This was the original plan and it is wrong:
`obs/fs` only brackets *caged* actors; coding harnesses run outside the cage, so the
camera never witnesses their edits (settled, `codeagent.rs:2731`). The correct,
already-wired mechanism is `auto_claim_write` driven by each harness's OWN write-tool
events:
- claude: the `Write`/`Edit` PreToolUse hook → `auto_claim_write` (`codeagent.rs` ~6957).
- codex: `file_change` summary items → `auto_claim_write` (`codeagent.rs:6069`).
- opencode: `edit`/`write` tool events → `auto_claim_write` (`codeagent.rs:5709`).
All three already collapse a touch into a `code_claims` row in the session's room,
tested (`codeagent.rs` ~10803 codex, ~10857 opencode). So codex/opencode siblings DO
get "last editing <path>" — there is no mechanism to add.
- **Interactive-TUI status (resolved after the journey-12 adjudication):**
  - *opencode TUI* — was the headless-only gap; now FIXED: the live SSE handler calls
    `auto_claim_write` on each settled `edit`/`write` part (`codeagent.rs`,
    `run_opencode_tui_server_events` SSE loop), parity with the headless `run` cell.
    An interactive opencode sibling now auto-claims in real time.
  - *codex TUI* — fixable via codex's HOOK system (corrected: codex DOES have hooks;
    an earlier claim that it didn't was wrong). `PostToolUse` fires on `apply_patch`
    in the interactive TUI, delivering the patch on stdin (verified 0.141.0:
    `tool_name=apply_patch`, `tool_input.command` = the `*** Add/Update/Delete File:`
    patch text; config is nested `[[hooks.PostToolUse]]`+`[[hooks.PostToolUse.hooks]]`,
    run with `--dangerously-bypass-hook-trust`). The fix is a **codex hook bridge**
    mirroring the Claude HookBridge: generate a codex hooks config in the per-session
    `CODEX_HOME`, point `PostToolUse` at `elanus code hook`, parse the apply_patch
    paths, `auto_claim_write`. The session id rides ENV (`ELANUS_CODE_SESSION`), not
    the payload's native `session_id`. The post-hoc rollout import stays as a
    legible-result fallback. (The headless codex worker already auto-claims via its
    `file_change` stream.)
- _(History: an earlier attempt added an `obs/fs`→claim fold in `code_projection.rs`;
  it was redundant + built on the false premise and has been removed. The dead
  `codesession.rs` helpers it needed — `auto_claim_room_and_workdir`,
  `workdir_room_id`, `peer_claims_fresh` — were removed too.)_

### SI4 — Change attribution backend (`whose-change`)
Answer "which of these dirty files are mine, and who owns the rest?" — the question
my human and I both got wrong about the codex WIP.
- A query/CLI that maps a path (or the working tree's `git status` set) → owning
  session via `code_claims` (the auto-claims `auto_claim_write` already records for
  all three harnesses from their write-tool events, plus manual `elanus code claim`),
  freshest-claim-wins.
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
