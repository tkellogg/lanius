---
status: done
author: Opus (planner) under Fable
last-updated: 2026-07-02
---

# Handoff: a one-shot scheduler, and letting an agent wake itself

Journey [../journeys/14-agent-messaging-itself.md](../journeys/14-agent-messaging-itself.md)
(stub): Tim types "schedule a message to post here in 5 seconds" and it happens —
the agent sets a timer, ends its turn, and *wakes itself* five seconds later to
act. Two things are missing today: there is **no one-shot scheduler** (packages
can declare recurring `cron`, but nothing fires a single event at a future time),
and — the subtler blocker — sprint 1's small-fixes M2 made the `emit_event` tool
**refuse all of `in/*`** (`src/exec.rs:1881`), so an agent can't even put mail in
its **own** mailbox to wake itself.

Tim's decision: **both** a kernel primitive and a skill. (a) A `schedule_event`
tool + an `elanus schedule` CLI, ledger-backed so it survives restarts, delivered
through the normal dispatch loop, reusing the existing cron **sweep** (the daemon
tick) rather than inventing a second clock. (b) A **guard carve-out**: an agent
may address its **own** mailbox (`in/agent/<its own noun>`); forging to *other*
agents' mailboxes, to `in/dm/*`, and to `in/human/*` stays refused. (c) A skill
teaching the primitive **and** the OS fallbacks (`at`/`launchd`/`sleep`) for
contexts with no bus (coding workers). (d) The whole loop proven end-to-end.

## Wonky bits / decisions to confirm

1. **The guard nuance: self yes, others/dm/human no.** The `emit_event` arm
   (`src/exec.rs:1881`) refuses everything under `in/`. Relax it to permit exactly
   the running agent's own mailbox — `type == crate::topic::agent_mailbox(&prof.
   agent)` (`src/topic.rs:304`, `in/agent/<noun>`) — and keep refusing every other
   `in/...`, including `in/agent/<someone-else>`. `prof.agent` is the kernel-known
   noun of the running agent (already in scope in the arm), so "self" is not
   agent-forgeable. security.md entry 15's real threat is *cross-agent* recall
   poisoning and forging *owner* mail; a self-addressed wake is neither. *Fable:
   confirm self-only via `prof.agent` equality (not a prefix check — `in/agent/`
   as a prefix would let `in/agent/victim` through).*

2. **One-shot is a new table swept by the SAME tick, not a second clock.** The
   `crons` table (`src/db.rs:223`) holds *recurring* 5-field cron expressions
   (`tick_crons`, `src/dispatcher.rs:596`); a one-shot "fire once at absolute time
   T" doesn't fit that schema. So add a small `scheduled_events` table and a
   sibling sweep `tick_schedules` **called from `tick()` right next to
   `tick_crons`** (`src/dispatcher.rs:254`). Same clock (the daemon tick), same
   ledger-durability + idempotency pattern — a sibling sweep, not a rival
   scheduler. *Fable: confirm a new table over overloading `crons` with a
   one-shot marker (I judged the recurring-cron schema a bad fit for absolute
   times).*

3. **Authorization is decided at schedule time (the tool), the fire is a kernel
   emit.** `tick_crons` re-checks `packages::may(publish)` at fire time
   (`src/dispatcher.rs:617`) because a cron belongs to a package whose grants can
   change. A `schedule_event` self-wake's authority is simply "may I message
   myself" — decided **once**, at the tool arm, where the actor's identity is
   known and trustworthy (`prof.agent`), exactly the doctrine small-fixes chose
   for `emit_event` (guard at the agent-reachable surface, not deep in `emit`).
   The sweep then fires as a plain kernel `events::emit` with an idempotency key.
   Store `created_by` on the row for provenance/audit regardless. *Fable: confirm
   schedule-time authorization over a fire-time re-check — the fire has no
   reliable actor to re-authorize (`ELANUS_ACTOR` is self-reported, entry 15).*

4. **The wake needs a handler subscribed to `in/agent/<noun>` — which is exactly
   why (c) exists.** A fired `in/agent/main` is picked up by the `chat` package
   (`packages/chat/elanus.toml`, `subscribe = ["in/agent/main"]`) → the daemon's
   `matching_exec_handlers` → an agent turn. `drive_code_deliveries`
   (`src/dispatcher.rs:1233`) only drives events it recognizes as *coding-session*
   deliveries (`recognize_delivery`), so a normal `in/agent/main` falls through to
   the exec handler — good (verify in impl). But a **coding worker** has no daemon
   handler on its mailbox, so a self-scheduled bus event would never wake it. That
   is the honest reason (c) teaches the OS fallbacks (`at`/`launchd`/`sleep`) for
   bus-less contexts. *Fable: confirm the split — bus primitive for daemon-driven
   agents, OS fallbacks for coding workers.*

5. **The self-wake carries a prompt + the conversation's session, so the agent
   acts and the result threads back to the right chat.** The scheduled event's
   payload is `{ prompt: "<what to do on wake>", session: <the turn's
   conversation> }`. On wake the agent runs a turn with that prompt and calls
   `send_message`; because ambient-conversations M1 stamps the run's session and
   the send threads onto the correlation, the message lands as a **replyable**
   thread in the person's chat ([ambient-conversations.md](ambient-conversations.md)).
   *Fable: an alternative is a "just emit this exact text at T" shortcut that skips
   the wake turn (cheaper — no LLM call) — but that throws away the whole point of
   journey 14 (the agent waking and acting is the feature). I default to the
   wake-and-act shape; confirm.*

**Product language.** Person-facing: "schedule", "reminder", "in 5 seconds",
"post here". Never "in/agent", "correlation", "emit", "mailbox", "ledger" in the
interface ([../layering.md](../layering.md)). The agent-facing skill is builder-
altitude and may name the primitives.

## Milestones

### M1 — The guard carve-out: an agent may wake its own mailbox
In the `emit_event` arm (`src/exec.rs:1870-1904`), before the blanket `in/`
refusal (`:1881`), permit `type == crate::topic::agent_mailbox(&prof.agent)`;
keep refusing every other `in/...`. Update the comment (it currently justifies
refusing *all* `in/`). Update security.md entry 15 (`docs/security.md:335-345`) to
record the carve-out: self-addressing is permitted (agent continuity, journey 14);
cross-agent / `in/dm/*` / `in/human/*` forgery stays refused — the entry's actual
threat is untouched.

**Acceptance:** extend `emit_event_refuses_reserved_ingress_plane`
(`src/exec.rs:2635`): a call with `type = agent_mailbox(&prof.agent)` (the running
agent's own noun) **succeeds** and lands a ledger row; `in/agent/<other>`,
`in/dm/...`, and `in/human/<owner>` still **refuse** with no row; `obs/...` still
succeeds. `cargo test` green.

### M2 — The one-shot schedule primitive (table + sweep + tool)
- **Table** (`src/db.rs`, beside `crons` `:223`): `scheduled_events(id INTEGER PK,
  fire_at TEXT NOT NULL, emit_type TEXT NOT NULL, payload TEXT, created_by TEXT,
  fired INTEGER NOT NULL DEFAULT 0)`.
- **Sweep** `tick_schedules` (`src/dispatcher.rs`, modeled on `tick_crons` `:596`,
  called from `tick()` `:254`): select rows with `fired = 0 AND fire_at <= now`,
  `events::emit` each with `idempotency = Some(format!("sched:{id}"))` (dedupe
  across restarts, mirroring the cron idempotency at `:643`), then set `fired = 1`.
  Durable: rows persist; the `fired` flag + idempotency key make a mid-fire crash
  replay a no-op.
- **Tool** `schedule_event` (a new arm in `src/exec.rs` `run_tool`, and a `Tool::
  new("schedule_event")` def near `:1499`): args `{ in_seconds?: number, at?:
  rfc3339-string, message: string }` (require exactly one of `in_seconds`/`at`).
  Target is **self only** — `emit_type = agent_mailbox(&prof.agent)` (never taken
  from args), same self-guard as M1. Insert a `scheduled_events` row with the
  computed `fire_at`, `payload = { prompt: message, session }` (wonky bit 5),
  `created_by = prof.agent`.

**Acceptance:** a unit test — calling `schedule_event` with `in_seconds` inserts
one `scheduled_events` row targeting the caller's own mailbox with the right
`fire_at`; a row whose `fire_at` is in the past is fired by `tick_schedules`
(emits one `in/agent/<noun>` event, `fired` flips to 1) and a second sweep does
**not** re-fire (idempotent); a future row does not fire. A `schedule_event` that
tries to target another mailbox is refused. `cargo test` green.

### M3 — `elanus schedule` CLI (the operator/human surface)
Add `Cmd::Schedule` (`src/main.rs`, the `Cmd` enum `:55`, dispatch match `:751`+),
e.g. `elanus schedule --agent <noun> (--in <seconds> | --at <rfc3339>) --message
"<text>"`, inserting a `scheduled_events` row targeting `in/agent/<agent>`. This is
a trusted human/operator gesture ("CLI is the API"), so it may target any named
agent's mailbox (unlike the self-only tool). Keep it thin — one insert, reusing
M2's row shape.

**Acceptance:** `elanus schedule --agent main --in 2 --message "ping"` inserts a
row; running the daemon (or a direct `tick_schedules` in a test) fires an
`in/agent/main` event carrying `{prompt:"ping"|…, session}` after the delay; a
scheduled event survives a daemon restart (row persists, fires once on the next
tick after `fire_at`).

### M4 — The skill: the primitive + the OS fallbacks
Ship agent-facing guidance (recommend a small dedicated `self-scheduling` skill
package in `kits/core/packages/`, modeled on `escalate` — *Fable: or a section in
`comms-etiquette/SKILL.md`; I lean dedicated because the OS-fallback content is
substantial and coding-worker-facing, while comms-etiquette is peer-talk*):
- **The bus primitive:** `schedule_event` — an agent with bus authority (a
  daemon-driven profile agent) schedules a self-wake; on wake it acts and, if it
  wants to reach the person, calls `send_message`. Reference the guard (self only).
- **The OS fallbacks:** a coding worker (no daemon handler on its mailbox, wonky
  bit 4) uses the machine instead — `at`/`launchd` for "run later", `sleep N &&
  …` in a shell for short waits — and note these run *outside* elanus's record, so
  the worker should still emit an obs line so its trace stays honest.

**Acceptance:** the skill documents both paths, states plainly which contexts have
the bus primitive vs must fall back to the OS, and names the self-only guard. A
`elanus context render` (or skill-visibility check) shows it reaches the intended
agents.

### M5 — The lived loop, end to end (Tim's acceptance)
Prove the whole thing: in chat, "schedule a message to post here in 5 seconds" →
the agent calls `schedule_event` → `tick_schedules` fires `in/agent/main` after
~5s → the `chat` handler wakes the agent → it calls `send_message` → the message
lands in the person's chat as a **replyable (ambient) thread**
([ambient-conversations.md](ambient-conversations.md)).

**Acceptance:** an e2e / `ui.spec.mjs`-level test drives the loop on a short delay
and asserts the scheduled message appears in `#view-converse` as a message the
person can reply to (a source badge marking it agent-initiated, per ambient-
conversations M3), threaded to the originating conversation. Rebuild + re-embed
the SPA before running (web-embed staleness note in memory). If a full ui.spec
loop is flaky on timing, split: an integration test for schedule→fire→wake→send
on the ledger, plus a ui.spec assertion that the resulting ambient message renders
replyably.

## Read these first
- The why: [../journeys/14-agent-messaging-itself.md](../journeys/14-agent-messaging-itself.md)
  (self-messages as continuity; the `in/agent/main` incident; the guard nuance).
- The guard being relaxed: [small-fixes.md](small-fixes.md) M2 and its decision 1;
  the arm at `src/exec.rs:1870-1904`; the test at `:2635`; security.md entry 15
  (`docs/security.md:306`, update at `:335`).
- The sweep to mirror: `src/dispatcher.rs` — `tick()` `:242-266` (add
  `tick_schedules` at `:254`), `tick_crons` `:596` (the pattern: read rows, check
  time, `events::emit` with idempotency `:643`, mark fired), the cron table
  `src/db.rs:223`, cron sync `src/packages.rs:345`.
- Why the wake routes to the chat handler: `packages/chat/elanus.toml`
  (`subscribe = ["in/agent/main"]`), `drive_code_deliveries` `src/dispatcher.rs:1233`
  (only drives recognized coding sessions), the exec-handler dispatch `:1526`.
- The topic helpers: `src/topic.rs:304` (`agent_mailbox`), `:309`
  (`human_mailbox`).
- The send that closes the loop: `src/exec.rs:1905` (`send_message` arm),
  `emit_message` `:865`; [ambient-conversations.md](ambient-conversations.md)
  (unprompted send → replyable thread).
- The CLI shape to match: `src/main.rs` `Cmd` enum `:55`, dispatch `:751`+.
- The wording rule: [../layering.md](../layering.md).

## Log
- 2026-07-02 — Created from Tim's demo-day findings + journey 14. Grounded against
  the worktree: `emit_event` refuses all `in/` at `exec.rs:1881`; the recurring
  `crons` table + `tick_crons` sweep (`db.rs:223`, `dispatcher.rs:596`) is the
  pattern to mirror but not overload (one-shot ≠ recurring cron); `agent_mailbox`
  is `topic.rs:304`; a fired `in/agent/main` routes to the `chat` package's exec
  handler (not `drive_code_deliveries`, which only drives coding sessions);
  `send_message` + ambient-conversations already turn an unprompted send into a
  replyable thread, so M5 stands on landed sprint-1 work. Judgment calls for Fable:
  self-only guard via `prof.agent` equality not a prefix (1); a new
  `scheduled_events` table swept by the same tick, not a second clock (2);
  authorization at schedule-time not fire-time (3); bus primitive for daemon agents
  + OS fallbacks for coding workers (4); the self-wake carries a prompt and acts,
  rather than a text-only shortcut that skips the turn (5).
- 2026-07-02 — All milestones implemented and adversarially verified (Opus
  impl/verify under Fable orchestration); landed on sprint-recon-2026-07.
  Status flipped to done.
