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
