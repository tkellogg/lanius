# elanus web — the flight recorder you can talk to

A local web dashboard. The **pure-MQTT-client constraint** from ui/tui
carries over one hop: browsers can't speak raw TCP MQTT, so `server.mjs` is
the ordinary anonymous loopback MQTT 5 client, and the browser talks to *it*
— bus messages relayed over SSE, publishes accepted over POST. No sqlite, no
trace.jsonl, no privileged access. The only filesystem touches: this
directory's static files and `<root>/bus.toml` for broker discovery.

## Run

```sh
cd ui/web && npm install
node server.mjs --root /tmp/elanus-live        # or HARNESS_ROOT / --url mqtt://...
# → http://127.0.0.1:7180   (--port to change)
```

## What you're looking at

- **conversation** (left): the mailbox view. Your composes go to
  `in/agent/<agent>` as `{prompt}` with a generated `el-correlation`; the
  agent's replies come back as `in/human/#` mail (`{text}`) on the same
  correlation. Asks render as answerable cards — option buttons or free
  text — and close themselves if answered elsewhere (CLI, TUI).
- **telemetry** (right): every bus message, color-coded by verb — teal for
  `in/#` (work), dim for `obs/#`, **international orange strictly reserved
  for `signal/#`** (real flight recorders are orange; the algedonic channel
  earns the alarm color). Filters: all / work / tools / signals, plus pause.
  Tool calls get compact ⚙ call/result badges.
- **signal lamp** (top right): lights and pulses on any `signal/#`; click to
  acknowledge.
- The server keeps a 1000-message ring so a late-opened tab gets recent
  history (the bus itself retains only per-topic last values).

## Test

`npm test` — real daemon on a throwaway root, the server as its MQTT client,
a plain HTTP client as the browser: SSE relay, ask + answer round trip into
the ledger with correlation intact, wildcard-publish rejection, ring
catch-up. Not part of tests/e2e.sh (the repo gate stays node-free).
