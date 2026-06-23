---
status: planned
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-23
---

# Agent-comms package

Make inter-agent communication a **package that rides on memory blocks** — not a
new subsystem. Per the planning chat: *"there doesn't need to be a distinction
between comms & blocks; keep it simple and just have blocks."* So priority is a
property of a *block* (placement → injection vector, from
[memory-blocks.md](memory-blocks.md) M4), the "unread from agent Y" surface is a
*computed block*, and the comms know-how is a *skill*. The transport already
exists; this handoff is mostly content + one stage + one config knob.

**Hard dependency:** memory-blocks M1–M4. Ship the skill (C1) anytime; the
computed-block parts (C2–C4) wait for blocks. Tim: *"the block can get bolted in
late."*

## What already exists (the transport is done)

- **Per-agent mailbox**: `in/agent/<noun>/<session>` (`src/topic.rs`
  `agent_mailbox`); `elanus code deliver`/`spawn` send (`src/codeagent.rs`
  `deliver()` ~1033, `spawn()` ~1226); correlation threads request→response;
  failure-mail routes `{failed:true}` back on the correlation (`src/exec.rs`
  `report_agent_failure` ~295).
- **Inbox**: `inbox_for_session()` (`src/codesession.rs:376`, own-mailbox-only,
  env-scoped), `elanus code inbox`, `mark_inbox_seen` + `code_inbox_seen` table.
- **Shared channels already exist as rooms**: `in/group/<id>`,
  `code_room_members`, **workdir-as-room** (sibling-awareness), `peer_claims`
  (`codesession.rs:655`), `live_siblings` (`codesession.rs:158`). The journey's
  *"a shared channel a bunch of agents agree on, a topic specific to a git
  repo"* is the room.
- **The computed-unread block already half-exists**: `turn_injection()`
  (`codeagent.rs` ~2119) already emits `[elanus] N new messages + preview` each
  turn. This handoff *generalizes* that hardcoded text into a block that flows
  through the block machinery (and so inherits placement→vector for free).
- **Message priority exists as a field**: `events.priority` (`src/db.rs`,
  `idx_events_pending`) — used today only for queue ordering, not injection.

## Decisions to confirm (the wonky bits)

1. **Computed comms blocks are ephemeral, not table-backed.** Inbox status and
   shared-channel tails change every turn; producing them as a stage / injection
   computation that adds to the Doc (and, for coding agents, the M4 injection
   render) — without persisting a `context_blocks` row each turn — keeps it
   light. Persisted blocks stay for identity/learned prompts. Downstream sees no
   difference (the whole point of blocks). Confirm this split.
2. **Generalize `turn_injection`'s inbox section into the inbox block, don't
   double it.** Move the inbox text into the inbox→block producer so there is one
   path, not the injection *and* a block both rendering inbox. The memory note is
   already migrating to a block (memory-blocks M2); inbox follows the same move.
3. **Priority mapping is a small table, owner's call.** A delivered message's
   `priority` maps to a block placement/vector: e.g. `0` → next-turn, `≥N` →
   mid-cycle, `signal/` → algedonic. The thresholds are a profile/package config
   knob, not hardcoded. Confirm the default ladder.
4. **Shared-channel subscribe is opt-in per profile.** Surfacing a room's
   (`in/group/<id>`) traffic as a block means subscribing beyond the agent's own
   mailbox — a deliberate widening. Gate it behind a profile/package config
   (which rooms, how many recent messages) rather than auto-subscribing.

## Milestones

### C1 — Comms-etiquette skill (ships now, no block dependency)
A skill-only package documenting the comms surface for an agent: how to
`deliver`/`spawn`, how to read the `inbox`, when to set priority, shared-channel
etiquette, and the failure-mail contract. Pure prompt content (a skill is
stateless and ignorable — the right tool for know-how, per the journey's
contrast with blocks). Installs via the normal kit/package path.

**Acceptance:** a profile that includes the package sees the comms skill in its
skills inventory (`render_parts` skills list); the skill names the real verbs
(`elanus code deliver/inbox/spawn`) and the priority/shared-channel conventions.

### C2 — Inbox as a computed block
Replace `turn_injection()`'s hardcoded inbox text with a computed `inbox` block
produced from `inbox_for_session()` (unseen count + latest preview). For native
agents it's a stage adding to `doc.system`; for coding agents it's computed in
the M4 injection render. One producer, both surfaces.

**Acceptance:** a session with unseen mail shows an `inbox` block via
`context render` (native) and in the per-turn injection (coding agent), with the
same content the old hardcoded path produced; an empty inbox produces no block
(quiet turn preserved).

### C3 — Priority → injection vector
Carry a delivered message's `events.priority` into the inbox block's
placement/priority so blocks M4 routes it: normal mail → next-turn, high-priority
mail → mid-cycle (Claude Code `Pre/PostToolUse`, opencode `prompt_async`),
algedonic → the `signal/` plane. Codex degrades to next-turn (blocks M4 ladder).

**Acceptance:** a message delivered with high priority lands **mid-turn** in a
live Claude Code/opencode session (not just on the next prompt); a normal message
lands next-turn; the threshold is read from config, not hardcoded.

### C4 — Shared channel as a block
Let a profile opt into surfacing a room's recent traffic (`in/group/<id>`,
reusing the room/`live_siblings` machinery) as a `channel:<id>` computed block —
the journey's per-repo shared channel. Bounded (recent-N, configured), advisory.

**Acceptance:** two sessions configured into the same room each see a
`channel:<room>` block summarizing recent shared-channel activity; a session not
in the room sees nothing; the recent-N bound is enforced (no flooding).

## Read these first
- The why: [../journeys/11-profiles.md](../journeys/11-profiles.md)
  ("Inter-agent communication").
- The substrate this rides on: [memory-blocks.md](memory-blocks.md) (blocks must
  land first; C3 needs M4).
- The transport that already exists: `src/codeagent.rs` (deliver/spawn/
  turn_injection), `src/codesession.rs` (inbox/rooms/siblings), `src/topic.rs`
  (mailbox + `in/group`), [../topics.md](../topics.md) (the `in/`/`obs/`/`signal/`
  planes), and the sibling-awareness handoff
  [sibling-awareness.md](sibling-awareness.md) (workdir-as-room).

## Log
- **2026-06-23 — planning.** Reframed from "inter-agent comms subsystem" to "a
  package on blocks." The transport (mailbox, rooms, failure-mail, inbox) is
  already shipped; the new value is (a) the etiquette skill, (b) inbox as a
  computed block, (c) priority→vector, (d) shared channel as a block. Mid-turn
  delivery for C3 is de-risked by the injection spike recorded in
  [memory-blocks.md](memory-blocks.md)'s Log (Claude Code + opencode yes; Codex
  degrades).
