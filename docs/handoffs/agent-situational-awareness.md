---
status: planned
author: Claude Opus 4.8 (planner) — written as a debrief + proposal
last-updated: 2026-07-07
---

# Situational awareness — why an agent can't account for the other work, and the mechanism that would fix it

Tim's framing (2026-07-07): *"you don't know what the other code is. I'm running
everything inside lanius — there should be some mechanism that tells you how to
account for that other work. Lanius is a messaging system. Why can't you just
message the agents? Or spy on what they're doing? Maybe the only problem that
actually matters here."*

He's right, and this is the debrief. **The surprising finding: lanius already has
most of the machinery** (the sibling-intent SI1/SI2 arc and sibling-awareness SA
arc are shipped). The blindness is not "no mechanism" — it's **five specific gaps
where the mechanism doesn't reach**. This doc names each from the two incidents I
actually hit today, grounded in code, then specs the fix.

---

## Part 1 — What blinded me (two concrete incidents)

### Incident A: the live codex sibling `code-c1e9d9c2`
Every turn, an advisory note told me *"code-c1e9d9c2 is editing
`src/codeagent.rs` and `docs/handoffs/codex-cage.md`."* I never learned **what it
was trying to do**, whether it was **still alive**, and its **claim lingered** for
the entire session — yet Tim later said no workers were active. So I routed around
a file for a *dead* session's stale claim, on no intent.

### Incident B: the four leftover worktrees
`elanus-harness`, `elanus-codex`, `elanus-skills`, `elanus-sprint` — I had to run
**git archaeology** (`git branch --merged`, `git rev-list --count`,
per-worktree `git status`) to discover they were all merged-and-clean stale
leftovers. Nothing in lanius linked a worktree/branch to **the session that made
it, its goal, or its outcome**. Tim's own memory said one "needs a rebase" — stale;
it was fully merged. Neither of us could account for the work without digging.

---

## Part 2 — What already exists (so we extend, not duplicate)

- **Liveness, defined honestly** (`src/codesession.rs:124-140`): a sibling is LIVE
  iff `code_sessions.last_active` is within `LIVE_WINDOW_SECS` **and** its room
  membership `owner_pid` passes a signal-0 probe. A SIGKILL'd session ages out.
- **Sibling-intent SI2** (`sibling-intent.md`, done; `src/code_projection.rs:371-440`):
  a session's todo list is projected into `code_session_tasks` (text + status),
  from `tool/TodoWrite/*` (Claude) and `assistant/todo` (codex). opencode emits no
  todo → honestly blank.
- **Advisory edit claims M5** (`src/codesession.rs:667-808`, `db.rs:492`): a session
  announces `code_claims` in its room; peers read others' claims. Reaped with the
  session's membership **"at the next launcher/daemon boot"** (`codesession.rs:686`).
- **Ambient notes** (SA arc, `sibling-awareness.md`): the `[lanius siblings]` /
  `[lanius peers]` lines injected each turn.
- **Query/message surface**: `lanius code sessions | session <id> | whose | claims`,
  `lanius code ask <session>` (block for a live reply), and the raw bus
  (`lanius bus sub 'obs/agent/<noun>/<session>/#'`).

So I *could* have messaged the live sibling (`ask`) or watched its raw obs
(`bus sub`). The reasons I didn't are the gaps below.

## Part 3 — The five gaps (grounded root-cause)

1. **The ambient note carries claims + liveness, but NOT intent.** The thing I
   read every turn showed *"editing codeagent.rs"* (a `code_claims` row) and
   `last active 7s ago` — never the SI2 task text. The richest signal
   (`code_session_tasks`) exists but isn't in the note, and nothing prompted me to
   go pull it with `lanius code session <id>`. **The intent is captured but not
   surfaced where attention actually lands.**

2. **Intent is todo-emission-dependent, so it's often blank.** SI2 only has data
   if the harness emitted a todo (`TodoWrite`/`assistant/todo`). A codex or
   opencode run that never produces a todo list — the common case —
   shows *nothing*, even though **its launch task is known** (`lanius code spawn
   <tool> "<task>"` carries it). Baseline intent is thrown away.

3. **Stale claims/liveness lag.** Claims are reaped only "at the next
   launcher/daemon boot," not gated on the liveness probe **at display time**. So a
   dead session's claim shows as active indefinitely (Incident A). The note should
   never show a claim from a session that fails the liveness probe *now*.

4. **No durable session ↔ artifact ↔ outcome ledger.** SI/SA are about *live*
   siblings in a shared tree. A *past* session's git artifacts (a branch, a
   worktree) have no lanius record of who made them, why, or their terminal status
   (merged / abandoned / WIP). `code_sessions` knows a session *ran*; it does not
   know it created worktree `elanus-harness` for goal G and that G shipped. Hence
   the archaeology (Incident B). (The `wip-code-<session>-routing` branch name
   suggests this link was *intended* but never made first-class.)

5. **Spy/message ergonomics are thin.** `ask` blocks and has no liveness pre-check
   (asking a dead session hangs to timeout). There's no readable "what is it doing
   right now" digest — only raw `obs/#` frames a human/agent must parse. So
   "spy on them" is technically possible but practically unused.

---

## Part 4 — The mechanism (milestones)

The through-line: **make every unit of work — live session OR the artifacts a dead
one left — self-describing and queryable in one place, and push that into the
attention path.** Call it *session accounting*.

### M1 — Put intent in the ambient note (highest leverage, small)
- The `[lanius siblings]`/`[lanius peers]` note already runs each turn. Add each
  live sibling's SI2 task (`code_session_tasks` latest in-progress item) and, when
  absent, its baseline launch task (M2). So the line reads *"code-c1e9d9c2 (codex,
  12s ago) — 'harden codex cage sandbox' — editing codeagent.rs"* instead of just
  the claim.
- **Acceptance:** the ambient note shows intent (task text) for a sibling that has
  one; degrades to the launch task, then to "(no stated intent)", honestly.

### M2 — Baseline intent from the launch task (never blank, harness-agnostic)
- Record the task string from `lanius code spawn/deliver <tool> "<task>"` (and the
  first user prompt for interactive sessions) as a session's baseline intent,
  published retained on `obs/agent/<noun>/<session>/intent` and stored on the
  session record. SI2 todos refine it; they no longer gate it.
- **Acceptance:** a codex/opencode session that never emits a todo still shows its
  launch task as intent in `lanius code sessions` and the ambient note.

### M3 — Liveness-gate the display + eager claim reaping
- Filter claims/siblings by the liveness probe **at read time** (don't show a
  claim whose session fails signal-0 / is stale), and reap dead sessions' claims on
  a daemon tick, not only at boot.
- **Acceptance:** a SIGKILL'd session's edit claim disappears from peers' notes
  within one liveness window, not at the next launch.

### M4 — Session ↔ worktree/branch ↔ outcome ledger + `lanius code sitrep`
- Record, per session: the git worktree/branch it created (hook the
  `new-worktree.sh` / `git worktree add` path, or detect at launch) and a terminal
  **outcome** (active | merged | abandoned | WIP-stranded), derivable from
  `git branch --merged` + liveness. Expose `lanius code sitrep`: one view of every
  session and loose worktree/branch — intent, liveness, workdir/branch, outcome —
  the thing I reconstructed by hand.
- **Acceptance:** `lanius code sitrep` lists the four leftover worktrees as
  "merged, session dead → safe to remove" without any git archaeology; a live
  session shows "active, branch X, intent Y."

### M5 — Spy + message ergonomics
- `lanius code watch <session>`: tail a **readable digest** of a session's live obs
  (assistant messages + tool calls summarized), not raw frames — "spy on what
  they're doing" in one command.
- `lanius code ask` gains a liveness pre-check: fail fast (or offer async
  `deliver`) when the target is dead, instead of blocking to timeout.
- **Acceptance:** `watch` streams a legible activity digest; `ask` on a dead
  session returns immediately with "session not live" rather than hanging.

---

## Wonky bits / decisions
1. **M1+M2 are the 80/20.** Intent-in-the-note + never-blank-intent would have
   resolved Incident A alone. Do them first; they're small and ride existing
   projections + the existing note.
2. **M4 is the structural fix for Incident B** and the most work — it's the
   durable "account for all work" ledger. Worth it: it turns git archaeology into a
   query, and it's what Tim means by "account for that other work."
3. **Scope of "spy" (M5) vs privacy** — homogeneous authority (Tim's model: no
   trust boundary between his own agents, see [[tim-safety-audit-not-restriction]]),
   so watching a sibling is fine; the point is legibility, not permission.
4. **Don't reinvent the liveness definition** — reuse `codesession.rs:124-140`.

## Read these first
- `src/codesession.rs:124-140` (liveness), `:667-808` (rooms + claims),
  `SessionRecord:64-82`.
- `src/code_projection.rs:371-440` (SI2 intent projection).
- `docs/handoffs/sibling-intent.md`, `sibling-awareness.md`,
  `sibling-resolution-skills.md` (the shipped arc this extends).
- `scripts/new-worktree.sh` (the worktree-creation path M4 would hook).
- The `sibling-coordination` skill (the read side agents actually use).

## Log
- 2026-07-07 (Opus, debrief + planner): wrote this after being blind to a live
  codex sibling's intent (its claim lingered post-death) and having to git-
  archaeology four merged-and-stale worktrees. Root cause is NOT missing
  machinery — SI1/SI2/SA are shipped — but five reach gaps: intent isn't in the
  ambient note (M1), it's todo-gated so often blank (M2), dead claims linger (M3),
  there's no session↔artifact↔outcome ledger so past work needs archaeology (M4),
  and spy/message ergonomics are thin (M5). M1+M2 are the cheap high-leverage fix;
  M4 is the structural one Tim is really asking for.
