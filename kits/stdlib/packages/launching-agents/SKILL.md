---
name: launching-agents
description: Launch lanius agents yourself — discover profiles and packages with `lanius agent catalog`, then run (blocking) or spawn (durable, async). Native agents use the launch_agent tool; coding workers use `lanius code`. Covers launch-time --with-package and --provider.
---

# launching-agents

You can launch other agents. This is first-class: introspect what's
available, pick a profile, and either run it in the foreground or spawn it
to run durably in the background. There is no trust boundary between your
own agents — the safety here is the audit trail (who launched what), not a
gate.

## 1. See what you can launch

```sh
lanius agent catalog          # human-readable
lanius agent catalog --json   # machine-readable: pick a profile + its packages
```

The catalog lists **native profiles** (each with its agent noun, model,
whether it is `spawn-ready`, and the packages visible to it), the
**coding tools** you can launch as workers, and the **providers** you can
target. A profile is `spawn-ready` only when an approved exec package
subscribes to its mailbox; a `run-only` profile can still run in the
foreground.

## 2. Run vs spawn

- **`run`** — blocking, foreground, any profile. You get the turn's output
  inline. Use for a quick synchronous sub-task.
  ```sh
  lanius agent run --profile helper "summarize docs/security.md"
  ```
- **`spawn`** — durable, async, daemon-driven. Returns immediately with
  `{event, correlation, session, mailbox}`; the result (or a failure) comes
  back later as mail on that correlation. Use for background work — and
  **do not block waiting**; end your turn.
  ```sh
  lanius agent spawn --profile helper "watch the build and report"
  ```

### If you are a native agent: the `launch_agent` tool

Native agents launch peers with the **`launch_agent`** tool, not by
hand-emitting a mailbox — a raw `emit_event` to `in/agent/<other>` is
refused (the in/ plane is reserved ingress; you may only wake your own
mailbox). `launch_agent` is the sanctioned door: it validates the profile,
gates on the exec handler, and threads provenance for you.

```json
launch_agent {
  "profile": "helper",
  "prompt": "<a self-contained task>"
}
```

It is async and returns `{correlation, session, mailbox}`. The launched
run is attributed to you.

### If you are a coding worker: `lanius code`

A coding session shells out. `lanius code spawn <tool> "<task>"` launches an
async worker; `lanius code <tool> --headless "<task>"` runs one blocking.
See the `lanius` worker-dispatch skill (`lanius code help`).

## 3. Launch-time overrides

Both `run`/`spawn` (and `launch_agent`) take two overrides that apply to
**that run only** — no durable config change, the profile.toml is untouched:

- **`--with-package <name>`** (repeatable; `with_packages` on the tool) —
  widen the run's VISIBLE packages to include an already-**approved** package
  the profile's path doesn't carry. This adds visibility, never authority:
  the package's bus capabilities stay gated by the grants ledger. An
  un-granted or uninstalled package is refused (`lanius approve <name>`
  first). Prefer this over editing a profile just to borrow one skill.
- **`--provider <name>`** (`provider` on the tool) — pin the model provider
  for the run (a name from `lanius provider list`), e.g. spawn a worker on a
  cheaper endpoint. Overrides the profile's `[model].provider`.

```sh
lanius agent spawn --profile helper \
  --with-package history --provider deepseek \
  "explain what session s-abc123 did to src/reactor.rs"
```

## See also

- **`explain-session`** — the ready-made recipe for dispatching a read-only
  reader at a dead session's history.
- **`lanius agent <verb> --help`** — the authoritative flag reference.
