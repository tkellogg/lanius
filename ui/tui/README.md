# elanus TUI

An ink-based terminal UI that is a **pure MQTT 5 client** on the loopback
listener. No sqlite, no trace.jsonl, no privileged access — if this UI ever
needs privilege, the bus design has failed. That constraint is the point:
the TUI is the proof that external clients get a complete window into the
system through the bus alone.

## Run

```sh
cd ui/tui && npm install
node index.js --root /tmp/elanus-live     # or ELANUS_ROOT, or --url mqtt://127.0.0.1:18830
```

The broker address comes from `<root>/bus.toml` (the one allowed filesystem
touch — config discovery), overridable with `--url`.

## Panes & keys

- **stream** — live event feed (`obs/#`, `in/#`, `signal/#`); signals are
  loud. Filters: `a` all, `t` tool calls, `w` work (`in/#`), `s` signals.
- **asks** — `in/human/#` messages accumulate as pending asks. `↑/↓` select,
  `enter` opens the answer editor, `enter` again publishes. An `in/agent/#`
  event arriving with a pending ask's correlation (e.g. a CLI answer) marks
  it answered.
- **compose** — publish new agent work to `in/agent/<agent>` as
  `{prompt: ...}`; QoS 1, the ✓ means the broker's PUBACK arrived (= the
  ledger accepted it).
- `tab` cycles panes, `q` quits.

## Wire notes

- Messages on the bus are envelope JSON: `{ts, kind, payload, event_id?,
  correlation_id?, cause_id?}`.
- Answers publish `{answer}` to the agent mailbox with the ask's correlation
  riding the **`el-correlation` user property** — the broker materializes it
  into the ledger event's `correlation_id`, which is what resumes the
  suspended asker. (MQTT Correlation Data is reserved for the resident-hook
  round trip; see docs/topics.md, "IDs: three layers".)

## Test

`npm test` — spins a real daemon on a throwaway root and drives the rendered
TUI through receive/ask/answer/compose, with a second MQTT client observing
that the answer lands on `in/agent/main` carrying the right correlation.
Deliberately not part of tests/e2e.sh: the repo gate stays node-free.
