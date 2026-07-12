# docs

Design records for elanus. Use this folder as progressive disclosure: start
with the doc that owns the question, then follow links only when the task needs
that layer.

## Start here

- Product or UI language: read [layering.md](layering.md), then
  [journeys/README.md](journeys/README.md) for audience fit and
  [ui-flows/README.md](ui-flows/README.md) for browser-flow assertions. For a
  deep investigation of behavior that does not yet have a decided fix, start
  at [bugs/README.md](bugs/README.md).
- Bus, broker, topic routing, packages, recorder, hooks, or delivery contracts:
  read [bus.md](bus.md); read [topics.md](topics.md) when the topic grammar or
  mailbox shape matters.
- Agents, actors, packages as actors, ingress/egress, or model/provider shape:
  read [actors.md](actors.md).
- Identity, credentials, sender provenance, owner naming, phonebook, recall, or
  egress provenance: read [identity.md](identity.md), then
  [security.md](security.md) if the question touches an open risk.
- Package configuration, profile edits, approvals, stdlib, config proposals, or
  autonomy levels: read [config.md](config.md).
- Context programs, context stages, block/register substrate, session/run
  vocabulary, or provider-request assembly: read [context.md](context.md).
- Filesystem grants, leases, write cages, fs events, or read/egress sandbox
  gaps: read [sandbox.md](sandbox.md).
- MCP integration or tool-server trust: read [mcp.md](mcp.md), then
  [security.md](security.md) entry 8.
- Early v1 architecture or historical build order: read [init.md](init.md).
  Skip it for current implementation questions unless you need the old design
  record.
- Debugging a *running* instance — where live state is, why obs aren't showing,
  the live-root-vs-repo distinction, leaked test processes: read
  [runtime.md](runtime.md).

## Contents

- [actors.md](actors.md) - actor model direction: everything addressable is an
  actor; packages can carry skill and actor roles; ingress is event-shaped and
  egress is command-shaped.
- [bus.md](bus.md) - MQTT 5 boundary architecture, planes, broker, hooks,
  recorder, packages, grants, and migration notes. It is the root for most
  runtime architecture questions.
- [config.md](config.md) - package and agent configuration model, Git-backed
  proposals, acceptance, autonomy, stdlib, and product-facing configuration
  language.
- [context.md](context.md) - context program doctrine, agent/session/run terms,
  context-stage resolution, block/register substrate, wire validation, and
  observability.
- [identity.md](identity.md) - broker-stamped identity, fenced credentials,
  owner naming, phonebook model, recall trust rules, and built increment notes.
- [init.md](init.md) - v1 minimal harness handoff. Useful for original
  principles, but superseded in places by later docs.
- [layering.md](layering.md) - kernel/building-block/product layering and the
  rule that product UI must translate internal vocabulary.
- [mcp.md](mcp.md) - MCP as a border protocol for third-party tool servers, not
  first-party elanus mechanisms.
- [sandbox.md](sandbox.md) - cage/camera split, grants, leases, fs event
  doctrine, read scoping + egress as the single-cage end state, the read camera
  (events on file access), and platform notes.
- [security.md](security.md) - index of known security issues and doctrine. Read
  this before claiming a security property or adding a privileged surface.
- [topics.md](topics.md) - v3 verb-first topic grammar, mailbox model,
  correlation taxonomy, and v2-to-v3 mapping.
- [runtime.md](runtime.md) - operating map of a running instance: the live root
  (`~/.elanus/root`) vs the repo, where `trace.jsonl`/`elanus.db` state
  materializes and the daemon dependency for recording, trace line format, and
  known leaked test-process cruft.
- [journeys/](journeys/README.md) - personas and product journeys for setup,
  coding agents (Codex and Claude Code), costs, risk/trust, and configuration.
- [ui-flows/](ui-flows/README.md) - executable web-flow catalog and QA findings.
- [bugs/](bugs/README.md) - evidence-led behavioral investigations: what the
  live product does, what the implementation means, and what remains unknown.
- [handoffs/](handoffs/README.md) - forward-looking implementation handoffs
  (work plans with milestones and acceptance criteria): the coding-agents
  envelope (Codex & Claude Code) and the configuration-UX altitude/scope pass.

## Implementation Anchors

- Bus, topics, hooks, recorder: [src/broker.rs](../src/broker.rs),
  [src/bus.rs](../src/bus.rs), [src/topic.rs](../src/topic.rs),
  [src/hooks.rs](../src/hooks.rs), [src/resident.rs](../src/resident.rs), and
  [src/recorder.rs](../src/recorder.rs).
- Events, ledger, dispatch, traces: [src/events.rs](../src/events.rs),
  [src/db.rs](../src/db.rs), [src/dispatcher.rs](../src/dispatcher.rs), and
  [src/trace.rs](../src/trace.rs).
- Config, packages, kits, manifests: [src/config_repo.rs](../src/config_repo.rs),
  [src/configcli.rs](../src/configcli.rs), [src/packages.rs](../src/packages.rs),
  [src/kit.rs](../src/kit.rs), [src/manifest.rs](../src/manifest.rs), and
  [src/profile.rs](../src/profile.rs).
- Identity and secrets: [src/secrets.rs](../src/secrets.rs),
  [src/paths.rs](../src/paths.rs), [packages/phonebook/](../packages/phonebook/),
  [packages/recall/](../packages/recall/), and
  [packages/webhook/](../packages/webhook/).
- Context pipeline: [src/context.rs](../src/context.rs),
  [src/context_blocks.rs](../src/context_blocks.rs), [src/render.rs](../src/render.rs),
  [src/exec.rs](../src/exec.rs), [packages/recent-history/](../packages/recent-history/),
  and [packages/window/](../packages/window/).
- Sandbox and leases: [src/sandbox.rs](../src/sandbox.rs) and
  [src/exec.rs](../src/exec.rs).
- Coding-agent launcher (`elanus code`, Claude Code adapter — launch, hook to bus
  bridge): [src/codeagent.rs](../src/codeagent.rs); the grant-scoped per-session
  identity (the broker resolves `code-*` as a scoped actor, not full authority) is
  [src/codesession.rs](../src/codesession.rs); see
  [handoffs/coding-agents.md](handoffs/coding-agents.md).
- Web product and QA flows: [ui/web/src/App.tsx](../ui/web/src/App.tsx),
  [ui/web/server.mjs](../ui/web/server.mjs), and
  [ui/web/test/ui.spec.mjs](../ui/web/test/ui.spec.mjs).
- Launching agents (agents launch agents, first-class):
  [src/agentcli.rs](../src/agentcli.rs) is `elanus agent` — `elanus agent catalog`
  inventories what you can launch (native profiles + the packages visible to each,
  coding tools, providers; `--json` for a machine-readable pick), `elanus agent run`
  executes a turn in the foreground (any profile, blocking), and `elanus agent spawn`
  queues a durable background turn on the profile's mailbox (async — needs an approved exec handler,
  i.e. `spawn-ready` in the catalog). Both `run` and `spawn` take launch-time
  overrides that apply to that run only, leaving `profile.toml` untouched:
  `--with-package <name>` widens the run's *visible* packages to an already-approved
  package (visibility, never authority — the grants ledger still gates bus actions),
  and `--provider <name>` pins the model provider. A **native** agent launches a
  peer with the `launch_agent` tool ([src/exec.rs](../src/exec.rs); a raw
  `emit_event` to another mailbox is refused, so this is the sanctioned door); a
  **coding** worker shells out via `elanus code` (see below). The `launching-agents`
  and `explain-session` skills in [kits/stdlib/](../kits/stdlib/) teach the how-to.
  See [handoffs/agent-launching.md](handoffs/agent-launching.md).
- Profile CLI: [src/profilecli.rs](../src/profilecli.rs) (`elanus profile`).
- Example kits and packages: [kits/funnel/](../kits/funnel/),
  [kits/memory-blocks-demo/](../kits/memory-blocks-demo/), and
  [packages/triage-demo/](../packages/triage-demo/).
