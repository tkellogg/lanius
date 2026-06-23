---
status: done — SA1/SA2/SA4 + SA3 write-half shipped; SA3 read-half deferred (rides read-provenance M2)
author: Opus 4.8 in Claude Code on elanus
last-updated: 2026-06-23
---
# Handoff: ambient sibling-awareness (introduce the agents on turn one)

Make a coding session **know who else is here** without being told. Today the
substrate exists — every session publishes its activity to the bus and there is a
durable `code_sessions` table and an advisory edit-claim mechanism — but a sibling
agent in the same working tree stays invisible until you trip over it (a shared
`git status`, a same-line collision). This handoff turns that ambient awareness on
**by default**, so two agents grinding the same repo divide the work on turn one
instead of reconciling it at commit time.

Answers Tim's question in `docs/_questions.md` ("agents are bumping into each
other … claude/codex agents need memory blocks injected by default that let them
know what else is going on … I'm not sure why any agent needs to have this turned
off"). It is the work plan behind the journey
[../journeys/09-colliding-with-a-sibling-agent.md](../journeys/09-colliding-with-a-sibling-agent.md),
whose three ascending rungs become this handoff's milestones.

> **Status: design + work plan. Not implemented.** The coordination *primitives*
> (claims, rooms, per-turn injection) are built and shipped (dispatch handoff M5);
> what is missing is making them **ambient and default** instead of opt-in and
> room-scoped.

## Read these first

- [../journeys/09-colliding-with-a-sibling-agent.md](../journeys/09-colliding-with-a-sibling-agent.md)
  — the why, first-person: how a session discovered its sibling at commit time and
  the three things that would each have surfaced it earlier. This handoff is that
  journey's "ascending order of ambition" made concrete.
- [coding-agent-dispatch.md](coding-agent-dispatch.md) — ships the primitives:
  `claim`/`unclaim`/`claims`, the `--room` model (M5), and the `turn_injection`
  seam that already carries inbox status, memory notes, and peer claims.
- [coding-agent-observability.md](coding-agent-observability.md) — the *human*-facing
  companion (obs → sqlite → web UI). Note its projection is **already partly built**
  in `code_projection.rs` (below); this handoff is the *agent*-facing read of the
  same facts.
- [../../src/codeagent.rs](../../src/codeagent.rs) — `turn_injection` (the per-turn
  `[elanus]` block, ~L1472), `take_room_flag` (the `--room` parse, ~L1026),
  `session_room_identity` (~L1331), and `claim_cmd`/`claims_cmd`.
- [../../src/codesession.rs](../../src/codesession.rs) — the **durable resume/room
  record**: `SessionRecord { elanus_session, native_session, tool, agent_noun,
  workdir, room }` (~L64). The `code_sessions` table behind it *also* has a
  `last_active` column the upsert bumps (~L107) — but it is **not** on the struct or
  in `read_record`'s SELECT (~L127), so SA2 must add it (or write a sibling query).
  Plus `code_room_members` (room, session, agent_noun, owner_pid) and `add_claim` /
  `peer_claims` / `own_claims`.
- [../../src/code_projection.rs](../../src/code_projection.rs) — the **obs projection
  that already exists** (distinct from the durable record above): the
  `code_session_stats` table (one row per session: `workdir`, `last_status`,
  `started_at`, `updated_at`, `parent`, model/effort/tokens) with a ready-made
  `list_sessions()` (~L479), and the `code_session_events` timeline. This is SA2's
  roster + liveness source. **Caveat:** it deliberately *ignores* `obs/fs/*` lines
  (test `ignores_non_coding_obs_lines`), so "which file is each sibling touching" is
  **not** in sqlite — that fact lives only on the live bus (the write camera) or in
  SA1's claims.
- [../security.md](../security.md) — the homogeneous-authority doctrine: claims are
  **advisory coordination, never authorization**. Two of the owner's own agents
  have the same authority; this handoff helps them *avoid conflict*, it does not
  invent a trust boundary between them.

## The gap today

The mechanism is there; the wiring is opt-in, so in practice it is off:

1. **Claims are room-scoped, and rooms are opt-in.** `session_room_identity`
   (codeagent.rs) *bails* if the session has no room, and a session only gets a
   room if it was launched with `--room <id>` (`take_room_flag`). Two agents in the
   same checkout that neither passed `--room` are in *no* room, so neither sees the
   other's claims. Journey 09: "Neither of us joined a room, so neither saw the
   other's claims."
2. **The per-turn injection never mentions live siblings.** `turn_injection` reports
   the session's own inbox, its memory note, and its *room* peers' claims — but if
   there is no room, the peer-claims branch is empty, and even with a room it only
   surfaces *claimed* paths, never "N other sessions are active here, and here is
   what they are touching." The one fact that would change behavior on turn one —
   *you are not alone in this directory* — is never said.
3. **Coordination depends on the agent remembering to speak.** A claim exists only
   if an agent runs `elanus code claim <path>`. An agent that just edits a file (the
   common case) announces nothing, so even a roommate sees it only if it volunteered
   a claim.

The result is exactly journey 09: reactive, late discovery — hand-staged commits,
dangling shared indexes, a mid-stream retreat into a worktree — for want of one
wire from facts the bus already carries into what a session passively sees.

## Why this is very doable (the facts already exist)

- **The roster already exists, materialized.** `code_projection.rs` keeps
  `code_session_stats` (one row per session: `workdir`, `last_status`, `updated_at`,
  `parent`) and exposes `list_sessions()`. "Other live sessions in this directory"
  is that list filtered to `workdir = ? AND elanus_session != ? AND last_status =
  'running'` (or `updated_at` within a window). No new table.
- **"What they're touching" is only half there.** Every session publishes
  `obs/agent/<noun>/<session>/tool/...` and the write camera publishes
  `obs/fs/<path>` — both live on the bus. But the projection **deliberately ignores
  `obs/fs`** today (test `ignores_non_coding_obs_lines`), so per-file touch is *not*
  in sqlite. SA2 gets the roster + status for free and sources "which file" from
  SA1's claims (cheapest, already a table) or a live `obs/fs` tail; projecting the
  fs stream is an optional later add, not a prerequisite.
- **The injection seam already runs every turn.** `turn_injection` is called per
  turn and is deliberately kept *out* of the cached prefix (it changes every turn),
  so adding a sibling line costs nothing in prompt-cache terms.
- **Claims already work** — they just need a default room and an automatic source.

## Design decisions (recommendations)

- **The workdir is the room.** Default a session's room to a stable id derived from
  its canonical `workdir` when no `--room` was passed, instead of leaving it
  None. Same checkout → same room → siblings see each other with **zero flags**.
  Explicit `--room <id>` still overrides (cross-workdir coordination, e.g. a planner
  grouping workers). A genuinely solo session in a unique directory gets a room with
  no peers — identical behavior to today, so nothing regresses for the solo case.
  *(This is the "self-navigate by default" Tim asked for: presence is structural,
  not a flag.)*
- **Default-on, quiet when alone.** Keep `turn_injection`'s "say nothing on a quiet
  turn" rule: the sibling line appears only when there *is* a live sibling. No
  firehose, no noise for the common solo session. There is no reason to expose an
  off switch (Tim: "I'm not sure why any agent needs to have this turned off") — if
  one is ever needed it is per-launch, not the default.
- **Advisory, never authorization.** Preserve the homogeneous-authority doctrine
  (security.md): a sibling line and a claim are *information an agent routes around*,
  not a lock and not a permission gate. No agent is ever *blocked* by a sibling.
- **Liveness, defined honestly.** "Live" = `last_active` within a window **and/or**
  `owner_pid` still alive (room members already carry `owner_pid`). A crashed
  session must age out of the roster so it does not haunt the injection forever.

## Milestones

### SA1 — the workdir is the room (ambient claims, no flag)
Default `room` to a canonical-workdir-derived id when `--room` is absent, in the
launch path that today calls `take_room_flag` and writes the `SessionRecord`. Relax
`session_room_identity` so `claim`/`unclaim`/`claims` work in the default room.
Explicit `--room` still wins.
**Acceptance:** launch two `elanus code claude` in the *same* directory with **no**
`--room`; after one runs `claim <path>`, the other's `elanus code claims` lists it
as a peer, and `turn_injection` surfaces it — with no flags passed by either.

### SA2 — live siblings in the per-turn injection
Extend `turn_injection` (codeagent.rs) to prepend, when present, a line naming the
*other live sessions in this room/workdir* and — where known — what each is
touching. Roster + liveness come from `code_projection::list_sessions()` filtered by
`workdir`/`last_status` (already built); "which file each is touching" comes from
SA1's claims (per-file touch is not projected — see the data note above). One line,
turn one.
**Acceptance:** with a sibling active in the same workdir, a session's `[elanus]`
block reads e.g. *"1 other coding session active here: code-abcd (claude-code),
last editing ui/web/App.tsx."* On a solo session the block is unchanged (no line).

### SA3 — touching a file *is* the claim (no remembering required)
Wire the fs cameras into automatic advisory claims so coordination stops depending
on an agent calling `claim`. The **write camera** is built (sandbox.rs; `obs/fs/<path>`
events on the bus) and can drive auto-claims for *writes* now; the **read camera**
(separate work — see sandbox.md "The read camera", `[OPEN]`) extends this to reads
when it lands. Mechanism: a small consumer tails `obs/fs/#` and calls `add_claim`
for the touching session — the projection does **not** capture these lines
(`ignores_non_coding_obs_lines`), so this is a new bus subscriber, not a projection
read. A caged session that touches `path` then has an advisory claim on `path` in
its room without ever running `claim`.
**Acceptance:** an agent that edits a file *without* ever running `elanus code
claim` still appears, for that path, in a roommate's `claims` and per-turn
injection. (Reads covered once the read camera ships; this milestone depends on a
read-camera handoff for the read half — track the dependency, don't duplicate it.)

### SA4 (optional) — proactive isolation nudge
When SA2 detects a sibling **in the same working tree** at session start, have the
briefing/injection *suggest* `git worktree` — the exact muddle journey 09 hit
("retreat into a separate `git worktree`"). Advisory only; never auto-creates or
blocks. Skips when siblings are in distinct worktrees of one repo (no shared index
to collide on).
**Acceptance:** starting a second session in a checkout that already has a live
sibling surfaces a one-line worktree suggestion; starting one in its own worktree
does not.

## Honesty / guardrails

- **No new bus authority, no cage change.** This is reads of existing roster/claim
  facts plus an extra injection line. No token, no permission, no sandbox change
  (the homogeneous-authority stance is unchanged).
- **Don't bust prompt caching.** The sibling line lives in `turn_injection`, which
  is already excluded from the cached prefix. Keep it there; never fold live-session
  state into the cached system prompt.
- **Don't firehose.** Cap the roster line (count + the one or two most relevant
  siblings, not an unbounded list); never fire per file-read once SA3 lands (claim
  granularity is per-path, advisory, deduped — not per syscall).
- **Stale-session hygiene.** A dead session must age out of "live" (last_active
  window / dead `owner_pid`) so the injection never names a ghost.

## Open questions

- **Room scope: workdir vs repo.** Workdir-as-room catches the acute journey-09 case
  (one shared checkout). Two *worktrees* of one repo have different workdirs but
  share branches/history — a softer collision (a branch/index race, not a same-file
  one). Should there be a coarser repo-level (`git common dir`) awareness band
  beneath the workdir room? SA1 picks workdir; revisit if worktree-vs-worktree
  collisions show up in practice.
- **The shared *git index*, specifically.** Journey 09's worst pain was a shared
  index hunk holding both agents' edits to `README.md`s. File-path claims help, but
  the index is a single shared resource. Is there a lighter signal for "someone else
  has staged changes here" than full edit-claims? (Possibly out of scope — the
  worktree nudge in SA4 sidesteps it.)
- **"What they're touching" source of truth.** The roster/status is settled —
  `code_session_stats` via `list_sessions()`. The open part is *per-file* touch: it
  is **not** projected (the projection ignores `obs/fs`), so the choices are (a) lean
  on SA1's claims (simple, crash-safe, but only as fresh as claims), (b) tail
  `obs/fs` live (precise, but a new subscription), or (c) extend the projection to
  capture per-session fs touches (most work). SA2 should start with (a); SA3 may
  motivate (b)/(c).
- **Codex parity.** The injection seam differs by harness (Claude system-reminder
  vs the codex `[elanus]` resume block — see harness-modes). Confirm the sibling
  line rides both; it should, since it reuses `turn_injection`.

## Log

- 2026-06-21 — Written from Tim's `docs/_questions.md` item ("agents are bumping into
  each other") and journey 09. Grounded against the code: the primitives exist
  (`claim`/`claims`, `--room`, `turn_injection` already carries peer claims) but are
  opt-in and room-scoped, so two flagless sessions in one checkout are mutually
  invisible. The gap is *default + ambient*, not new mechanism. SA3's read half waits
  on the read camera (sandbox.md `[OPEN]`).
- 2026-06-21 — Verification pass before handing this off cold. Corrected three things
  a fresh implementer would have tripped on: (1) `SessionRecord` has **no**
  `last_active` field — it is a `code_sessions` *column* only, absent from the struct
  and `read_record`'s SELECT, so SA2 must surface it; (2) the obs projection already
  exists in `code_projection.rs` (`code_session_stats` + `list_sessions()` +
  `code_session_events`) — SA2's roster/liveness is mostly built, just uncited
  before; (3) the projection **ignores `obs/fs`** (verified by
  `ignores_non_coding_obs_lines`), so per-file "what they're touching" is not in
  sqlite — it lives on the live bus or in SA1's claims. The write camera itself is
  confirmed built (`sandbox.rs`, `obs/fs/<path>` events). Milestones and data-source
  notes updated accordingly.
