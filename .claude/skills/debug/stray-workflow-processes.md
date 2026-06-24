# Stray / orphaned `elanus` processes from workflow runs

## Symptom
Leftover `elanus` and python processes piling up over a session: bus noise (see
[broker-credential-refused.md](broker-credential-refused.md)), ports held, CPU/RAM
creep, or just a confusing `ps`. They are usually **orphaned** (`ppid 1`) and sit on
`/tmp` / `/private/tmp` roots.

## What it means
Test/probe/UI/adversarial **workflows start real elanus processes** — `elanus web`
or `elanus serve`/`daemon` plus package handler scripts (`history`,
`recent-history`, …) — in throwaway `/tmp` roots to exercise the server. When the
dispatching subagent exits without reaping them, they **orphan and keep running**.
Each `elanus web` also spawns its package-handler children, so one stray run can
leave a half-dozen processes. Across a long session, dozens accumulate.

This is the runtime twin of the git-pollution failure mode in the
`adversarial-workflow-containment` memory — same root cause (workers not cleaning up
what they start), different mess.

## Diagnose
```sh
ps -eo pid,ppid,command | grep '[e]lanus'
```
Split by root:
- **Legit** — under your real root (default `~/.elanus/root`): the `serve` →
  `daemon` → `web` trio and its package handlers. LEAVE THESE.
- **Stray** — `-C /tmp/elanus-*` / `/private/tmp/elanus-*` roots, usually `ppid 1`.
  Names like `elanus-probe.*`, `elanus-p404.*`, `elanus-web-smoke.*`,
  `elanus-verify-*`, `adv-*` betray the workflow that spawned them.

```sh
# just the strays:
ps -eo pid,command | grep '[e]lanus' | grep -E '/tmp/elanus|/private/tmp/elanus'
```

## Fix
Reap the strays by **explicit PID** (after eyeballing each), sparing the legit
stack:
```sh
kill <pids…>        # clears the python handlers
kill -9 <web-pids…> # `elanus web` ignores SIGTERM — force the survivors
```
Then re-run the `ps` to confirm zero `/tmp`-root elanus processes and that
`serve`/`daemon`/`web` are still alive.

**Gotcha — `pkill -f` may be refused.** The auto-mode safety classifier blocks a
blanket `pkill -f elanus…` (it can't verify the processes are "yours"), but it
**allows a surgical `kill <explicit-pids>`** once you've `ps`-verified each is a
`/tmp`-root orphan. So: enumerate → verify → kill by PID. If a kill is still
denied, hand the human the exact `kill -9 <pids>` line to run.

## Prevent
- Workflows that start servers/daemons must **reap them with `kill -9`** before the
  agent exits (bake it into the worker prompt; note that `elanus web` survives
  SIGTERM).
- Bind any workflow-started broker/web to a **per-root non-default port** so a leak
  can't touch the real bus.
- Prefer **deterministic tests** over standing servers where possible (see the
  `adversarial-workflow-containment` memory).
