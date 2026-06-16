# elanus

A minimal event-driven agent harness: `inetd + cron + git hooks + sqlite + a
flight recorder`. The kernel is an event log, a trace log, a dispatcher, and
two narrow contracts (handler execution, render provider). Everything else is
userland: skill packages containing executables.

Design doc: [docs/init.md](docs/init.md).

## Build & init

```sh
cargo build --release
export PATH="$PWD/target/release:$PATH"

elanus init ~/agent          # scaffolds db, trace, default profile, stock skills
export ELANUS_ROOT=~/agent
export ANTHROPIC_API_KEY=...  # any genai-supported provider works; see profile.toml
```

## Run

```sh
elanus daemon &                                  # the dispatcher (supervisor, not doer)

elanus exec --session hi "hello"                 # chat = exec with a session id
elanus emit work/agent/exec --payload '{"prompt":"summarize echo.log"}'   # async agent turn
elanus emit work/demo/echo --payload '{"x":1}'        # any event; handlers.d decides who cares

elanus inbox                                     # what's blocked on you?
elanus answer 42 "yes, ship it"                  # answers route by correlation_id
elanus events --limit 30                         # debug view of the log
elanus render | less                             # inspect assembled context
tail -f $ELANUS_ROOT/trace.jsonl | jq .          # the flight recorder
```

## The milestone loop

A cron tick wakes the agent → it works → hits a question → emits `human/ask`
and exits 75 (checkpoint-and-exit; the transcript in sqlite *is* the process
state) → notify pops a macOS notification → you `elanus answer` → the
dispatcher matches the correlation_id and re-invokes the handler with the
answer → it finishes. If the deadline passes first, the declared default is
applied and the assumption is logged as an ordinary event — auditable,
vetoable.

`tests/e2e.sh` exercises exactly this loop (no API key needed).

## Skill packages

A skill package is a directory in `$ELANUS_ROOT/skills/`, per the
[agentskills.io](https://agentskills.io) spec, optionally extended with a
sibling `harness.toml` manifest:

```toml
[[handler]]
on = "work/discord/message"     # topic filter, wildcards ok ("signal/#")
run = "scripts/reply"      # any language; event JSON on stdin
order = 0                  # cross-package ordering

[[cron]]
schedule = "*/5 * * * *"   # 5-field (seconds-resolution 6-field also ok)
emit = "feeds.check"

[[provider]]
run = "scripts/context"    # contributes a context block at render time

[throttle."work/discord/#"]
max_concurrent = 2
```

`elanus enable <name>` materializes the manifest into `handlers.d/` symlinks
(systemd-enable style); the manifest is the source of truth, `handlers.d/` is
the compiled routing table, and debugging is `ls`. `SKILL.md` (agent-facing
instructions) and `harness.toml` (dispatcher-facing wiring) never mix.

Stock packages: `chat` (work/agent/exec → agent turn), `notify` (asks/signals →
macOS notification), `watchdog` (cron monitor emitting `signal/pain` on
failures — measured pain, not self-reported), `echo` (demo), `notes`
(instructions-only skill).

## Handler contract

- Event JSON envelope on stdin (`{"resume": <answer event>}` added on resume).
- Env: `ELANUS_EVENT_ID`, `ELANUS_CAUSE_ID`, `ELANUS_CORRELATION_ID`,
  `ELANUS_DB`, `ELANUS_TRACE`, `ELANUS_ROOT`, `ELANUS_PROFILE`,
  `ELANUS_RESUME=1` on resume.
- Exit 0 done; exit 75 suspended (emit a `human/ask` with a correlation_id
  first — that's the resume key); anything else failed.
- Emit follow-up events with `elanus emit`; `cause_id` threads automatically
  from the environment.

## Trace log

`trace.jsonl` is append-only and write-only — nothing reads it for control
flow. One JSON object per line: `dispatch`, `handler.exit`, `llm.request`,
`llm.response`, `tool.call` (written *before* execution), `tool.result`,
`emit`, `signal`, `expire`. Thinking is excluded (not evidence); full
transcripts live in the `messages` table.

## Status / non-goals (MVP)

- Sandbox is the VM preset only: the box is the boundary. `[sandbox]` in
  profile.toml is parsed, not enforced.
- One daemon per root; don't run two (no lock yet).
- KB-in-git, channel adapters beyond macOS notifications, and indexer packages
  are userland exercises the contracts already support.
