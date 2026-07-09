<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/tkellogg/lanius/main/brand/logos/09-lockup-dark.svg">
    <img src="https://raw.githubusercontent.com/tkellogg/lanius/main/brand/logos/09-lockup-light.svg" alt="lanius" width="340">
  </picture>
</p>

<p align="center"><em>A local control plane for AI work.</em></p>

<p align="center">
  <a href="https://crates.io/crates/lanius"><img src="https://img.shields.io/crates/v/lanius.svg" alt="crates.io"></a>
  <a href="https://github.com/tkellogg/lanius/actions/workflows/ci.yml"><img src="https://github.com/tkellogg/lanius/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/tkellogg/lanius/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="license: Apache-2.0"></a>
</p>

Lanius is a local control plane for AI work.

It is built around one idea: the human should stay in charge of attention,
budget, and intent. Models, scripts, coding tools, webhooks, and background
workers should do the right-sized part of the job and then report back.

The point is not to make one giant agent that does everything. The point is to
make it easy to use the cheap thing for cheap work, the fast thing for live
interaction, and the strong thing when judgment actually matters.

## Install

Install the released binary with Cargo:

```sh
cargo install lanius
```

Then create a local root and start the app:

```sh
lanius init
lanius serve
```

Open the web UI:

```sh
open http://127.0.0.1:7180
```

`lanius serve` runs the dispatcher and the embedded web app. You do not need a
repo checkout, Node, or Vite for normal use.

By default, your lanius root lives at `~/.lanius/root`. To use a different one:

```sh
export LANIUS_ROOT=~/my-lanius-root
lanius init "$LANIUS_ROOT"
lanius serve
```

## What you use it for

Use lanius when you want AI tools to work around you instead of through you.

- Start Claude Code, Codex, or opencode with shared context and observability.
- Run multiple coding sessions in the same repo without using yourself as the
  coordination layer.
- Let one worker hand a narrow task to another worker.
- Keep a live UI, terminal session, script, webhook, or notification channel in
  the same message space.
- Add packages that bring skills, memory, background processes, and integrations.
- See what happened after the fact: sessions, messages, telemetry, asks, and
  failures.

The human-facing model is simple:

1. You start the system.
2. You launch or talk to a worker.
3. Lanius records what is happening.
4. Workers can message you or each other.
5. You approve the parts that need human judgment.

## First run

Start with the web UI:

```sh
lanius init
lanius serve
open http://127.0.0.1:7180
```

From there you can:

- chat with the default profile;
- create or edit profiles;
- configure model providers;
- inspect coding sessions;
- review package settings and grants;
- answer messages that are waiting on you.

You can do the same things from the CLI. The UI intentionally shells through the
same `lanius` commands, so there is one path for human actions.

## Model providers

For normal chat/profile agents, add a provider and point a profile at it.

Example Anthropic-compatible provider:

```sh
export ANTHROPIC_API_KEY=...
lanius provider add anthropic \
  --wire anthropic \
  --base-url https://api.anthropic.com \
  --key-env ANTHROPIC_API_KEY

lanius provider test anthropic
lanius profile set default model.provider=anthropic model.model=claude-sonnet-4-6
```

For coding tools, you can also use their native login instead of an API key:

```sh
lanius provider add claude-login --native --tool claude
lanius provider add codex-login --native --tool codex
```

The web UI has provider setup as well. Use whichever path is less annoying.

## Coding with lanius

Run coding tools through lanius from the project you want them to work on:

```sh
cd ~/code/my-project

lanius code claude
lanius code codex
lanius code opencode
```

Those commands launch the real tools. Lanius does not fake a coding agent. It
wraps the session so the work becomes visible, addressable, and resumable.

Run a headless worker when you want a narrow task completed without opening a
TUI:

```sh
lanius code codex --headless "run the test suite and fix the first failure"
lanius code claude --headless "review this diff for behavioral regressions"
```

Inspect what is going on:

```sh
lanius code sessions
lanius code sitrep
lanius code claims
lanius code whose --dirty
```

Watch a running session:

```sh
lanius code watch <session-id>
```

Open a specific session report:

```sh
lanius code session <session-id>
```

That report includes the tool, status, timeline, changed files, and the exact
resume command when lanius knows one.

## Dispatching work

The most useful pattern is not "one assistant does everything." It is:

- one worker plans;
- another worker implements;
- another verifies;
- you make the calls that actually need you.

Inside a lanius-launched coding session, a worker can start another worker:

```sh
lanius code spawn codex "implement the parser change described in docs/parser.md"
```

It can also send a message to an existing worker:

```sh
lanius code deliver <worker-session> "please check whether your change touches auth"
```

The worker's completion comes back through the requester's mailbox. That means a
planner can hand off implementation, end its turn, and wake back up when the
worker is done.

For humans, the important bit is that the handoff is visible. You can inspect the
sessions, see what files changed, and intervene without becoming the message bus
yourself.

## Messages and human attention

Lanius uses a mailbox model. Anything can send a message if it has the authority:
a profile, a coding session, a package daemon, a webhook, a CLI command, or the
web UI.

See what is waiting on you:

```sh
lanius inbox
```

Answer a question:

```sh
lanius answer <ask-id> "yes, ship it"
```

Send a message directly to a profile:

```sh
lanius emit in/agent/main --payload '{"prompt":"summarize what changed today"}'
```

Schedule a wake-up:

```sh
lanius schedule --agent main --in 3600 --message "check whether the build finished"
```

This is where interaction models fit. A fast voice or chat model does not need to
be smart enough to write code, debug a system, or make every decision. It can
translate human intent into a message for the worker that is actually good at the
task.

## Packages

Packages are how lanius grows without turning into a giant core.

A package can include:

- instructions for an AI tool;
- memory blocks;
- knowledge bases;
- background processes;
- cron jobs;
- tools;
- config fields;
- coding harness adapters;
- message subscriptions and publish grants.

List packages:

```sh
lanius packages
```

Check package validity:

```sh
lanius packages check
```

Approve a package's requested authority:

```sh
lanius approve <package-name>
```

Revoke it later:

```sh
lanius revoke <package-name>
```

Install a kit, which is a bundle of packages and profiles:

```sh
lanius kit list
lanius kit show core
lanius kit add core
```

Install with review first:

```sh
lanius kit add core --pending
lanius packages
lanius approve <package-name>
```

Useful starting points:

- `core` teaches workers how to coordinate, escalate, estimate work, and use
  lanius itself.
- `funnel` demonstrates right-sized work: scripts drop obvious noise, a cheap
  model reviews the survivors, and only the interesting items reach a human.
- `helper` adds a helper profile and knowledge about the local lanius setup.

## Practical workflows

### 1. Use a coding tool normally, but make it observable

```sh
cd ~/code/my-project
lanius code claude
```

Work as usual. Then inspect:

```sh
lanius code sessions
lanius code sitrep
```

This is the lowest-friction way to start. You still use the tool you already
use, but lanius can see the session, changed files, claims, and messages.

### 2. Run a cheap worker for a narrow job

```sh
lanius code codex --headless "update the README examples to match the new CLI"
```

Use this for work where a frontier model would be overkill. The worker can still
be observed, resumed, and reviewed.

### 3. Split planning and implementation

Start a strong planning session:

```sh
lanius code claude
```

Then have it spawn a cheaper implementation worker:

```sh
lanius code spawn codex "implement the migration described in the plan"
```

The planner does not have to sit there while the worker runs. Lanius carries the
completion message back.

### 4. Build a funnel

Install the funnel kit:

```sh
lanius kit add funnel
```

Then feed it noisy text. The first stages are deterministic and cheap; the model
only sees the residue. This is the same pattern you want for feeds, inboxes,
alerts, issue queues, and research streams.

### 5. Keep durable memory outside any one worker

Use knowledge bases and blocks when something should survive a session:

```sh
lanius kb list
lanius kb search "release checklist"
lanius block set project-style "Prefer small, reviewable changes."
```

The point is not to make every prompt huge. It is to make the right context
available to the right worker when it needs it.

## The mental model

Lanius is shaped like a small local operating system for AI work:

- `in/...` topics are mailboxes.
- `obs/...` topics are telemetry.
- `signal/...` topics are alarms.
- SQLite is the durable ledger.
- The trace log is the flight recorder.
- Packages are userland.
- The web UI and CLI are human control surfaces.

If you want the deeper mechanics, start here:

- [docs/topics.md](docs/topics.md) - topic grammar and mailbox model
- [docs/actors.md](docs/actors.md) - actors, scripts, humans, and models
- [docs/context.md](docs/context.md) - how context is assembled
- [docs/config.md](docs/config.md) - profile and package configuration
- [docs/coding-harness-onboarding.md](docs/coding-harness-onboarding.md) -
  adding a new coding tool adapter
- [docs/security.md](docs/security.md) - current security notes and known gaps

## Developing lanius

A repo checkout needs Rust and Node (the web UI is built by `build.rs` and
embedded into the binary; installed releases ship it prebuilt).

```sh
cargo run -- dev
```

That starts the dispatcher, Rust web relay, and Vite UI with restarts:

```text
web relay: http://127.0.0.1:7180
Vite UI:   http://127.0.0.1:5173
log:       target/lanius-dev.log
```

For a production-style local run from a checkout:

```sh
cargo run -- serve
```

Run tests:

```sh
cargo test
tests/e2e.sh
```

The old low-level tagline still applies:

```text
inetd + cron + git hooks + sqlite + a flight recorder
```

It is just not the first thing a human should have to understand.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
