---
name: debug
description: >-
  Runbook for diagnosing common elanus RUNTIME / operational failures — broker &
  bus errors ("CONNECT refused", "bad credential"), stray/orphaned processes,
  ports stuck in use, the daemon not dispatching, the web UI not loading. Use when
  something is failing at runtime (not a compile error or a logic bug) and you want
  the symptom → cause → diagnose → fix → prevent steps. Each file in this skill is
  ONE runbook entry, keyed by the symptom you actually see. Start here, match your
  symptom to an entry, follow it.
---

# debug — elanus operational runbooks

When elanus misbehaves at runtime, match what you SEE to a runbook entry below and
follow it. Each entry is self-contained: **Symptom → What it means → Diagnose →
Fix → Prevent.** These are for *operational* failures (processes, the bus, the
daemon, credentials) — not compile errors or code logic.

## The one diagnostic you'll run first, almost every time
Separate the **legit stack** from **strays**. The healthy stack is three nested
processes under the *real* root (default `~/.elanus/root`):

```
elanus serve                                   # the supervisor
└─ elanus -C ~/.elanus/root daemon …           # the broker + dispatcher
└─ elanus -C ~/.elanus/root web --port 7180    # the web server (+ package handlers)
```

List everything and split it by root:
```sh
ps -eo pid,ppid,command | grep '[e]lanus'
```
Anything on a `/tmp` or `/private/tmp` root (often `ppid 1`, orphaned) is a
**stray** — almost always left behind by a test/probe/workflow run, not part of the
real stack. Most runtime weirdness traces back to strays.

## Runbook entries
- [broker-credential-refused.md](broker-credential-refused.md) — the broker log
  repeats `[bus] CONNECT refused: bad credential for identity "<name>"`. Usually a
  stray client authenticating against the wrong root — NOT a broken broker.
- [stray-workflow-processes.md](stray-workflow-processes.md) — orphaned `elanus`
  web/daemon/handler processes from workflow runs piling up: bus noise, ports held,
  CPU. How to enumerate and reap them safely (and why `pkill` may be refused).
- [daemon-stack-health.md](daemon-stack-health.md) — "is elanus even up?" Deliveries
  stuck, agents not dispatching, the UI not loading. Confirm the serve→daemon→web
  stack and the broker port.

## Adding a runbook entry
Hit a new failure, fixed it, want it to be a 30-second lookup next time? Add
`<symptom-slug>.md` here with the same shape (Symptom / What it means / Diagnose /
Fix / Prevent), ground it in the real log string + file anchors, and link it from
the list above. Keep entries keyed by the **symptom a human actually sees**, not the
internal cause — that's what you'll be grepping for at 2am.
