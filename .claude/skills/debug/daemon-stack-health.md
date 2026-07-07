# "Is lanius even up?" — daemon / stack health

## Symptom
Deliveries sit `pending` and never run; agents don't dispatch; `lanius code
deliver`/`spawn` go nowhere; the web UI won't load or its live stream is silent;
or operational credential/connection errors appear out of nowhere. (Several of the
credential errors seen in practice were simply **the daemon being down**, not a
code bug.)

## What it means
The work plane is a supervised stack; if a layer is missing, work stops moving.
The healthy shape:
```
lanius serve                                   # supervisor — owns the others
└─ lanius -C <root> daemon --interval-ms 1000  # broker (MQTT) + dispatcher tick
└─ lanius -C <root> web --port <p>             # web server (+ package handlers)
```
- The **daemon** runs the broker (binds the MQTT port — default `127.0.0.1:1883`,
  see `bus.toml`) AND the dispatch tick that announces ledger events and drives
  deliveries. No daemon → nothing gets announced or driven; `in/` events just queue.
- The **web** server is the human seat + `/api/stream`; it connects to the broker.

## Diagnose
1. Is the stack up?
   ```sh
   ps -eo pid,ppid,command | grep '[e]lanus'
   ```
   You want `serve`, `daemon`, and `web` all present, all under your real root
   (default `~/.lanius/root`), with `daemon`/`web` parented by `serve`. Missing
   `daemon` is the usual culprit.
2. Is the broker actually listening?
   ```sh
   lsof -nP -iTCP:1883 -sTCP:LISTEN      # or your configured bus.toml port
   ```
   Expect the `daemon` PID. Nothing listening → broker down.
3. Check the supervisor log for why it died/refused:
   ```sh
   tail -n 100 ~/.lanius/root/lanius-serve.log
   ```
   (Credential refusals here usually mean *clients*, not the daemon —
   see [broker-credential-refused.md](broker-credential-refused.md).)

## Fix
- Daemon/stack down → (re)start the supervisor: `lanius serve` (it brings up the
  daemon + web). Confirm with the `ps` above.
- Port already held (broker can't bind) → something else is on the bus port; find
  it (`lsof -iTCP:<port>`) — often a **stray** from a workflow run; reap it (see
  [stray-workflow-processes.md](stray-workflow-processes.md)) and restart.
- Deliveries still stuck with the stack healthy → look at the ledger/dispatcher,
  not this runbook (that's a logic issue, not an operational one).

## Prevent
- Don't run two stacks against the same root/port. Workflow-started instances must
  use a per-root non-default port (this also avoids the credential-refusal noise).
- After a crash, prefer one clean `lanius serve` over hand-starting `daemon`/`web`
  separately, so the supervisor owns lifecycle + restarts.
