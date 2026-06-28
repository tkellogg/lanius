---
status: draft
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-28
---

# Handoff: sibling-resolution skills — what to DO when siblings' work tangles

The agent-facing half of [journeys/12-knowing-what-a-sibling-is-doing.md](../journeys/12-knowing-what-a-sibling-is-doing.md).
[sibling-intent](sibling-intent.md) makes *what each sibling is doing + when it was
last active* ambient; this handoff ships the skills that **cue a session to read
that surface and resolve a conflict deliberately** instead of reverse-engineering it
from `git diff` and guessing. It rides the already-shipped
[agent-comms](agent-comms-package.md) mailbox/room rails and the
[sibling-intent](sibling-intent.md) surfaces.

These skills are exactly the four "moves" I had to *be* by hand during the
commit-the-codex-WIP incident (journey 12): read a sibling's status, attribute a
diff, ask a sibling, and run the resolution decision tree.

## Package shape (recommended)

**One package, `sibling-coordination`** (a `kits/core/packages/` skill, peer to
`comms-etiquette`), whose `SKILL.md` is the resolution **playbook** and whose four
"moves" are its sections. This mirrors `comms-etiquette` (one package, one SKILL.md
covering several related moves) and keeps the decision tree in one place where the
agent reads it as a unit. It carries **no `elanus.toml` manifest** — like
`comms-etiquette`, it's pure skill content gated by profile visibility; it requests
no new bus authority because it only invokes existing/added `elanus code` CLI verbs.

*Alternative considered:* four separate skill packages (`sibling-status`,
`whose-change`, `ask-sibling`, `resolve-sibling-conflict`) for finer progressive
disclosure. Rejected for the first cut — the moves are one short decision flow and
splitting them buries the umbrella (`resolve-sibling-conflict`) that ties them
together. Split later if the SKILL.md grows past a screen.

Materialization: once installed on the default profile, the
[coding-skill-materialization](coding-skill-materialization.md) work already
delivers it into claude (`--plugin-dir`), codex (`CODEX_HOME`), and opencode
(`OPENCODE_CONFIG_DIR`) sessions automatically. No per-harness work here.

## Two new CLI verbs this skill needs

Most of the substrate exists (`elanus code rooms`/`sessions`/`claims`/`deliver`/
`inbox`). Two gaps:

1. **`elanus code whose <path>` / `elanus code whose --dirty`** — change attribution.
   Specified in [sibling-intent](sibling-intent.md) **SI4**; the skill consumes it.
2. **`elanus code ask <session> "<question>" [--timeout N]`** — a blocking
   deliver-and-wait. Today `elanus code deliver` sends and returns immediately, and
   the reply lands in `inbox` on the correlation; an agent must hand-roll the
   poll-for-reply loop. `ask` wraps it: `deliver` with a fresh correlation, then
   block up to `--timeout` (default ~20s) polling `inbox_for_session` for the
   correlated reply, print the reply or "no answer — treat as contended." Thin over
   the shipped rails (`deliver` `codeagent.rs:1205`, `inbox_for_session`
   `codesession.rs:381`, correlation threading already present). High-priority
   variant (`--priority 5`) interrupts a live sibling mid-turn (the comms mid-cycle
   vector) rather than waiting for its next turn.

## The four moves (and how each maps to the substrate)

| Move | What it does | Backed by |
|---|---|---|
| **sibling-status** | "What is each live sibling doing, and is it still alive?" | `elanus code sessions` / `rooms` enriched by sibling-intent SI1 (last-active) + SI2 (task list). Degrades to `rooms` roster + obs tail without SI. |
| **whose-change** | "Which dirty files are mine; who owns the rest?" | `elanus code whose --dirty` (SI4). Degrades to `elanus code claims` + manual `git diff` reasoning without it. |
| **ask-sibling** | "Ask a live sibling a scoped question, wait briefly for the answer." | `elanus code ask <session> "<q>"` (new verb above) over the shipped deliver/inbox/correlation rails. |
| **resolve-sibling-conflict** | The decision tree that sequences the other three and decides commit / stash / leave / worktree / ask-human. | Pure content; cues the above + `git worktree` + `ask_human`. |

## Drafted `SKILL.md` (the deliverable content — refine in review)

```markdown
---
name: sibling-coordination
description: What to do when you share a working tree with other coding sessions — how to see what each sibling is doing and when it was last active (sibling-status), figure out which uncommitted changes are yours vs theirs (whose-change), ask a live sibling a question and wait for the answer (ask-sibling), and the decision tree for resolving a tangle without clobbering another agent's work (resolve-sibling-conflict). Read this the moment `git status` shows changes you don't recognize, before committing/stashing in a shared tree, or when the [elanus siblings] note says another session is active.
---

# Coordinating with sibling coding sessions

You may share one working tree (and one git index) with other coding sessions. The
`[elanus siblings]` note each turn names who is live, their tool, when each was last
active, and what each is working on. When their work and yours tangle, do NOT
reverse-engineer it from `git diff` or commit a dirty tree blind. Use these moves.

## sibling-status — what is each sibling doing?
- `elanus code sessions` — every live coding session: tool, last-active, current task.
- `elanus code rooms` — your room's roster + who is claiming/editing which files.
- Read the per-turn `[elanus siblings]` note first; these commands are the detail.
- "last active 30s ago" → actively working, treat its files as hot. "last active
  40m ago / off the roster" → likely stranded; its work may be safe to take over.

## whose-change — which changes are mine?
- `elanus code whose --dirty` — annotates `git status` with the owning session,
  tool, last-active, and current task for each changed file.
- `elanus code whose <path>` — for one file.
- A file with no owner but in your diff is yours. A file owned by a live sibling is
  theirs — do not stage it as if it were your work.

## ask-sibling — just ask
- `elanus code ask <session> "are you still editing src/foo.rs? safe for me to
  touch src/bar.rs?"` — sends the question and waits briefly for the reply.
- Add `--priority 5` to interrupt a live sibling mid-turn for an urgent question.
- No answer in time → treat the contended file as theirs and route around it.
- You can also be asked: if a sibling messages you, answer it (`elanus code inbox`).

## resolve-sibling-conflict — the decision tree
1. `git status` shows changes you don't recognize? Run `whose-change`.
2. All yours → proceed normally.
3. Some are a sibling's:
   - **Never** `git commit`, `git add -A`, `git stash`, or `git checkout` over a
     **live** sibling's uncommitted work. That clobbers or finalizes work you didn't
     do and can collide with its in-flight edits.
   - Sibling **live** (last active recently)? `ask-sibling` whether it's safe, or
     retreat: do your own work in a `git worktree` (`git worktree add ../wt -b mine`)
     and merge later. Claim your files (`elanus code claim <path>`) so it routes
     around you.
   - Sibling **stranded** (off the roster / long-idle, work left dangling)? It may be
     safe to commit its work to preserve it — but only with the human's explicit
     OK, and commit it **attributed to that session/work**, never as your own.
   - Editing the **same file** as a sibling? Stop. `worktree` or divide the file by
     section after an `ask-sibling`. Two agents on one file's lines is the one
     collision git won't save you from.
4. When in doubt, surface it to your human with the `whose-change` breakdown rather
   than guessing. A deliberately-incomplete commit of only your own files is safer
   than a complete one that swallows a sibling's work.

## Why this exists
A real session was told "commit your work," found a codex sibling's 600-line
in-flight change in the tree, and had to deduce all of this by hand. These moves
make that a glance, not an investigation. See
docs/journeys/12-knowing-what-a-sibling-is-doing.md.
```

## Verification
- **Skill loads + materializes:** install `sibling-coordination` on the default
  profile; launch `elanus code {claude,codex,opencode}` and confirm the agent can
  read the skill (the coding-skill-materialization e2e already proves the delivery
  path per harness). Don't reship that machinery — just confirm the new package
  appears in each harness's skill list.
- **`elanus code ask` round-trips:** session A `ask`s session B; B replies on the
  correlation; A's blocking call returns B's reply within the timeout; A times out
  cleanly to "no answer — treat as contended" when B is silent.
- **`elanus code whose --dirty`:** with a sibling's uncommitted change present, the
  command attributes each changed file to its owner (depends on SI4).
- **Scenario replay (the incident):** reproduce the codex-WIP situation — a codex
  sibling leaves an uncommitted change; a claude session runs the playbook end to
  end (`sibling-status` → `whose-change` → decision) and reaches "this isn't mine,
  it's code-XXXX's authority-delegation work, still in_progress → don't commit, ask
  or worktree" **without** any `git diff` archaeology. That this took me a dozen
  manual steps is the acceptance bar.

## Dependencies & sequencing
- **Hard:** the comms rails ([agent-comms](agent-comms-package.md)) — shipped.
- **Soft (graceful degradation):** [sibling-intent](sibling-intent.md) SI1/SI2/SI4.
  Build sibling-intent first for the full experience; the skill is still useful
  against today's `rooms`/`sessions`/`claims` (it just lacks last-active, task
  lists, and `whose --dirty`, falling back to manual `git diff` for attribution).
- The `elanus code ask` verb can ship with this package (it's comms-layer) or fold
  into sibling-intent — implementer's choice; it's small either way.

## Anchors
- `kits/core/packages/comms-etiquette/{SKILL.md,elanus.toml}` — the package pattern
  to copy (skill-only, no bus authority).
- `src/codeagent.rs` — `deliver` (1205), `spawn` (1408), the `elanus code` verb
  dispatch; add `ask` and `whose` alongside.
- `src/codesession.rs` — `inbox_for_session` (381), correlation/reply routing,
  `peer_claims` (720), `add_claim` (679).
- [coding-skill-materialization](coding-skill-materialization.md) — how this package
  reaches each harness once installed (no work needed here).

## Out of scope
- New transport or identity — rides the shipped mailbox/room rails.
- Per-harness skill delivery — already solved.
- A web UI for sibling coordination — follow-on.
