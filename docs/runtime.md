# Runtime layout & operating notes

Practical facts about a *running* elanus that aren't obvious from the source
tree. Read this when debugging live behavior, observability, or "why don't I see
the data I just produced." Design rationale lives in the other docs; this is the
operational map.

## The live root is not the repo

elanus stores all per-instance state under a **root directory**, and the running
system's root is almost never the source checkout.

- **Live/production root:** `~/.elanus/root`. The packaged stack runs there —
  `elanus -C ~/.elanus/root daemon` (the broker + recorder + dispatcher) and
  `node ui/web/server.mjs --root ~/.elanus/root` (the web relay), both launched by
  `elanus serve`.
- **The repo** (`/Users/tim/code/elanus`) is the source tree. It may contain its
  own *stale* `trace.jsonl` / `elanus.db` from old `cargo run`/dev sessions —
  these are NOT what a running system reads or writes. Do not debug against them.
- **A process's real root is its `ELANUS_ROOT` env** (or its `-C <root>` flag),
  not its cwd. A coding session launched with cwd in the repo still inherits
  `ELANUS_ROOT=~/.elanus/root` and reads/writes there. **Always resolve the root
  from `ELANUS_ROOT` before inspecting state** — `echo $ELANUS_ROOT`, then look
  under that path. (This bit one debugging pass: obs looked "missing" only because
  the repo's stale `trace.jsonl` was grepped instead of `$ELANUS_ROOT/trace.jsonl`.)

## Where state materializes, and the daemon dependency

Under the live root:

- **`trace.jsonl`** — the flight recorder (the recorder's `trace` sink). Append-only
  and **write-only**: nothing reads it for control flow. This is where `obs/...`
  observations land (coding-session telemetry included).
- **`elanus.db`** — the sqlite ledger. `in/#` and `signal/#` events are
  sqlite-backed via `emit()`; the durable `code_sessions` records live here too.
  `obs/` do **not** go to sqlite (only to `trace.jsonl`) — which is why queryable
  observability needs a materializer (see
  [handoffs/coding-agent-observability.md](handoffs/coding-agent-observability.md)).

**The recorder only runs inside the daemon.** A launcher (e.g. `elanus code …`)
*publishes* obs to the broker; the daemon's recorder *consumes* and writes
`trace.jsonl`. So **if the daemon is down, obs are published but never recorded** —
they simply don't appear in `trace.jsonl`. Confirm the daemon is up
(`pgrep -fl 'daemon --interval-ms'`) before concluding the recording path is broken.

**Trace line format** (for grepping): each line is a JSON object keyed by
`"kind"` (the topic), `"payload"`, `"sender"`, `"ts"`. Match
`"kind":"obs/agent/<noun>/<session>/..."` — there is no `"topic"` field. Example
coding-session leaves: `session/start` (now carries `parent`, `model`, `effort`),
`session/thread`, `tool/<name>/call`+`/result`, `assistant/message`,
`session/idle` (token `usage`), `session/resume`, `session/stop`.

## Known cruft: leaked test-harness processes

The Rust test/e2e harnesses (`funnel`, `e2e`, `linkroot`, `ui-spec`) create temp
roots under `/private/tmp/elanus-*` and start package daemons (`history`,
`recent-history`, Python scripts) inside them. On teardown those package
subprocesses are **not** reaped — they orphan to `ppid 1` and accumulate across
runs (observed: 50+ processes, ~1.8 GB of temp roots, 1–4 days old). They run
against their own temp roots and never touch the live root, so they are harmless
to correctness — just resource cruft.

- **Reap them:** `kill $(pgrep -f '/private/tmp/elanus-')` then
  `rm -rf /private/tmp/elanus-*` (safe — none is the live root or production
  daemon, which live under `~/.elanus/root`).
- **Root cause worth fixing:** test teardown should kill the package subprocesses
  it spawned (or run them under a process group it can signal), so a test run
  doesn't leak long-lived daemons.
