---
status: M1-M3 done (intent-in-note · baseline intent · tri-state broker liveness); M4 (sitrep ledger) + M5 (watch/ask) are follow-up
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

3. **Stale claims + binary liveness.** Two problems. (a) Claims are reaped only "at
   the next launcher/daemon boot," not at display time — so a dead session's claim
   shows as active indefinitely (Incident A). (b) Worse, liveness is **binary**
   (alive/dead) and **ignores the broker's own view** of connection. It should be
   **tri-state and broker-driven** — connected / disconnected / dead — so that a
   *disconnected* agent (which may be a live **split brain** still editing files)
   is never silently treated as dead and its claims never wrongly reaped. See M3.

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

### M3 — Tri-state liveness (connected / disconnected / dead), broker-driven
**Liveness is NOT binary.** The dangerous bug is treating a partitioned agent as
dead. Model three states:
- **connected** — the broker holds a live MQTT session for it AND (same host) its
  `owner_pid` passes signal-0 AND `last_active` is fresh.
- **disconnected** — the broker lost it (keepalive timeout / Last-Will fired), so
  it's off the bus — **but it may still be running.** A network partition is a
  **split brain**: the agent is alive, still editing files, just can't see or be
  seen by peers. **Disconnected is not dead.**
- **dead** — *confirmed* gone: a same-host `owner_pid` signal-0 probe fails, or a
  session sits disconnected past a grace window with its pid gone.

Requirements:
- **The broker is the shared source of truth for connection** (Tim: "if the broker
  recognizes the agent as disconnected, the other agents see it that way too").
  Give each session's MQTT client a **Last-Will-and-Testament** that publishes a
  *retained* `obs/agent/<noun>/<session>/status = {connected:false, ts}` on
  ungraceful disconnect; the session sets `{connected:true}` on connect and clears
  it on clean exit. Every peer subscribed to that topic then sees the exact state
  the broker sees, near-instantly — which also fixes the stale-claim lag (a
  disconnect is a keepalive away, not "next daemon boot").
- **Reap claims ONLY on confirmed *dead*.** A **disconnected** agent's edit claims
  **stay** — it might be a split brain still writing. The ambient note flags them:
  *"code-XXXX — disconnected (may still be running — possible split brain; treat
  its claims as live)."* Only a confirmed-dead session's claims reap.
- **Bias toward "might still be running."** Same-host death is confirmable (pid
  probe); a cross-host / partitioned session is **not** — it stays
  `disconnected (unknown)` indefinitely rather than being auto-reaped.
- **Acceptance:** a SIGKILL'd same-host session → **dead** (pid gone), claims
  reaped within a liveness window. A session whose broker connection drops while
  its process keeps running → shows **disconnected (possible split brain)**, its
  claims are **NOT** reaped, and the note warns peers. A clean exit → `connected:
  false` via LWT, seen by peers within one keepalive.

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
4. **Extend the liveness definition, don't fight it** — reuse `codesession.rs:124-140`
   as the "dead" (pid) half, and add the broker-connection half on top (M3).
5. **Disconnected ≠ dead is a safety rule, not a nicety.** The one thing that must
   never happen: treat a split-brained agent (alive, partitioned, still writing) as
   gone and reap its claims / assume its files are free. The broker's LWT gives a
   fast, uniform *disconnect* signal; only a same-host pid probe gives *death*.
   When in doubt, "still running." Cross-host death is unconfirmable — leave such a
   session `disconnected (unknown)` rather than reaping it.

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
- 2026-07-07 (Tim review): approved, with a correctness addition — **liveness must
  be tri-state, not binary.** Propagate the broker's own disconnect view to all
  peers (MQTT Last-Will → a retained `.../status` topic), and **separate
  "disconnected" from "dead": a disconnected agent may be a live split brain still
  editing files, so its claims must NOT be reaped until death is confirmed (same-
  host pid probe).** M3 rewritten accordingly; gap 3 and wonky-bit 5 added.
- 2026-07-07 (Opus impl + xhigh verify): **M1-M3 shipped.** Ambient note now shows
  intent (refined-todo → baseline launch task → honest "(no stated intent)");
  launch task recorded on the SessionRecord (`intent` column) + published retained
  on `.../intent`; a per-session MQTT **liveness beacon** with a retained
  Last-Will publishes `.../status={connected}`, mirrored into
  `code_sessions.connected`. Liveness is tri-state via `classify_liveness`; **claims
  reap ONLY on confirmed `Dead` (same-host pid gone)** — a `Disconnected(SplitBrain)`
  or `Disconnected(Unknown)` session keeps its claims and the note flags it. 571
  tests green incl. the safety test `reap_reaps_only_confirmed_dead_never_a_
  disconnected_split_brain`. Verifier confirmed no path treats a disconnect as
  death. Known residuals (minor, honest): cross-host peers don't yet subscribe the
  retained `.../status` and mirror it (same-host peers see it via the shared
  ledger; cross-host stays `Disconnected(Unknown)`, never reaped — safe); the
  `conn_updated_at` column is written but unused (no time-grace escalation, since
  the pid probe subsumes it). M4 (`lanius code sitrep` ledger) + M5 (`watch`/`ask`)
  deferred, noted in-code.
