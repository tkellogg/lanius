---
status: planned
author: Claude Opus 4.8 (planner)
last-updated: 2026-07-07
---

# Small fix: headless claude "writing adapter summary failed: No such file or directory"

## The bug

Every headless `claude` run logs:

```
[code] writing adapter summary <root>/run/<session>/adapter-summary.json failed: No such file or directory (os error 2)
```

and the worker's real result (final text + file changes) never reaches the parent —
the parent falls back to an empty summary.

## Root cause (an ordering bug — the scratch dir is removed before the summary is written)

The run scratch directory `root.run_dir().join(session)` is used by BOTH the parent
launcher and the child adapter, and the child destroys it before the summary is
written into it.

1. **Parent launcher** creates the scratch and hands the summary path to the child:
   - `src/codeagent.rs:3518-3520` — `let scratch = root.run_dir().join(&session); create_dir_all(&scratch)`.
   - `src/codeagent.rs:3522` — `let summary_path = scratch.join("adapter-summary.json")`.
   - `src/codeagent.rs:3657` — `cmd.env(ENV_SUMMARY_FILE, &summary_path)` (this is what
     `ctx.summary_file()` returns in the child).
   - After the child exits: `src/codeagent.rs:3685` reads the summary
     (`read_capture_summary_file(Some(&summary_path))`), then `:3686` removes the
     scratch (`let _ = std::fs::remove_dir_all(&scratch);`). This post-closure cleanup
     always runs (see the closure note at `src/codeagent.rs:3532`).

2. **Child adapter** (`run_claude_adapter`, `src/codeagent.rs:4104`) calls
   `run_claude_capture` (`:4109`), which:
   - re-derives the **same** path — `src/codeagent.rs:3919`,
     `let scratch = root.run_dir().join(session)` — writes `settings.json`/plugin into
     it, runs claude, and then **removes the whole scratch at
     `src/codeagent.rs:4070`** (`let _ = std::fs::remove_dir_all(&scratch);`) as its
     own cleanup, and returns.

3. Back in `run_claude_adapter`, `src/codeagent.rs:4123`:
   `write_capture_summary_file(ctx.summary_file(), &summary)` writes to
   `scratch/adapter-summary.json` (the write itself is
   `src/codeagent.rs:7432-7442`) — but that directory was just removed at step 2.
   `std::fs::write` fails with ENOENT → the logged error, and the parent's read at
   `:3685` gets nothing.

**Why it's claude-only** (matches "every headless *claude* run"): the codex and
opencode adapters (`run_codex_adapter` `src/codeagent.rs:4127`, `run_opencode_adapter`
`:4184`) also call `write_capture_summary_file` (`:4180`, `:4217`), but their capture
paths do NOT `remove_dir_all` the shared scratch — only `run_claude_capture` does,
prematurely.

## The fix (minimal)

Delete the premature scratch teardown in the child capture — the parent already owns
and removes that scratch.

- **Remove `let _ = std::fs::remove_dir_all(&scratch);` at `src/codeagent.rs:4070`**
  (inside `run_claude_capture`). The parent launcher already tears down the identical
  `root.run_dir().join(session)` at `src/codeagent.rs:3686`, AFTER it has read the
  summary — so cleanup is preserved and the summary write at `:4123` now finds its
  directory intact. Semantically this is correct: the parent created the scratch
  (`:3519`), so the parent should own its teardown; the child's removal was both
  redundant and premature.

If the implementer prefers a self-contained fix that doesn't rely on the parent's
cleanup: keep `run_claude_capture`'s teardown but write the summary BEFORE it — thread
`ctx.summary_file()` into `run_claude_capture` and call `write_capture_summary_file`
just before line 4070. That's more code; the one-line deletion is the minimal fix.

## Milestone (single)

### Write the summary before the scratch is gone

**Acceptance:** a headless claude run (`lanius code claude --headless "<task>"`)
completes with **NO** `[code] writing adapter summary ... failed` line on stderr, and
`adapter-summary.json` exists in the run scratch where the parent reads it
(`read_capture_summary_file`, `src/codeagent.rs:3685` / definition `:7444`), so the
parent's printed worker result carries the worker's real final text and file changes
instead of an empty summary.

**Regression note:** the flow is cross-process (parent spawns child adapter), so the
cheapest deterministic guard is a unit test asserting the ordering invariant — e.g.
after `run_claude_capture` returns, the scratch dir still exists so a subsequent
`write_capture_summary_file(ctx.summary_file(), ...)` succeeds; or an e2e headless run
asserting `adapter-summary.json` is present and non-default and no failure line was
logged.

## Coordination flag (shared file)

`src/codeagent.rs` is currently claimed by a sibling coding session. Edit ONLY the
specific scratch/summary functions — `run_claude_capture` (the line 4070 teardown)
and, if taking the alternative, its summary write / `write_capture_summary_file`
(`:4123`, `:7432`). Expect to reconcile with the sibling's in-flight edits to this
file before committing (see the sibling-coordination skill).
