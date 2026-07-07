# dev kit — work on a codebase, with seatbelts

This kit sets lanius up to act as a coding agent on a repo of yours. The
flow:

1. **Point the agent at your repo.** In your profile's `[sandbox]` section
   set `workdir = "~/code/yourproject"` — the shell tool then runs there
   instead of the harness root. Workdir is *location*, not authority:
   writes still flow through the whole-agent grant (`fs_write`) and leases.
   A `profiles/dev/profile.toml` skeleton ships with this kit.
2. **The agent leases what it changes.** With `fs_write` granted, the agent
   acquires an exclusive write lease (`fs_lease` tool) on the subtree it's
   about to mutate; the spawn cage narrows to the lease, and concurrent runs
   cannot write into each other's subtrees.
3. **git-protect vetoes destructive git.** A `pre_tool_call` exec hook on
   the shell tool denies, with a reason naming the offending pattern:
   - `git push --force` / `-f` (`--force-with-lease` is allowed)
   - `git reset --hard`
   - `git clean -f` (any `-f...` cluster or `--force`)
   - `git branch -D`
   - `git checkout .` / `git restore .` (tree-wide discards)

   It parses conservatively: only confident matches deny; anything it cannot
   parse, and every non-git command, passes untouched. The deny lands in the
   model's transcript as an ordinary tool error (it can adapt) and on the
   flight recorder under `obs/harness/hook/pre_tool_call/deny`.

Try it:

```
lanius exec "summarize the repo layout" --session dev1
```
