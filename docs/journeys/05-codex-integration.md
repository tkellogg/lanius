---
name: Codex integration
description: Early design notes for integrating Codex without implementing it in the setup UI pass.
---

# Codex Integration

This is not implemented. It is a design sketch for later.

## Product Shape

The user-facing concept should be "coding agent" or "Codex terminal," not
"package." Daniel should understand why this is better than opening Codex
directly; Tim should still be able to inspect the underlying profile, package,
hooks, sandbox, and ledger events.

## Direction

`elanus codex` should launch the real Codex CLI/TUI with the real Codex user
experience and CLI options. Elanus should not reimplement Codex or wrap it in a
fake web UI.

The difference is the operating envelope:

- Elanus owns sandboxing.
- Elanus owns observation.
- Elanus owns durable event recording.
- Elanus owns message delivery into the running Codex session when possible.
- Codex remains the real actual Codex TUI.

Codex's native sandboxing needs research. The goal is to override it with
Elanus sandboxing where possible, while replicating enough of Codex's expected
sandbox semantics that Codex still behaves sensibly.

## Research Findings

Source: current Codex manual, sections "Sandbox", "Hooks", "Permissions", and
"CLI command reference" fetched through the OpenAI docs helper on 2026-06-19.

### Codex sandboxing

Codex sandboxing applies to commands spawned by Codex, not only to built-in file
edits. That includes shell commands, `git`, package managers, and test runners.

Codex uses platform-native enforcement:

- macOS: Seatbelt
- Linux and WSL2: `bubblewrap` when available, with a bundled helper fallback
- native Windows: Windows sandboxing for PowerShell, Linux sandboxing in WSL2

Codex separates sandboxing from approval policy. The sandbox is the technical
boundary; approvals decide when Codex pauses before crossing it.

The documented sandbox modes are:

- `read-only`
- `workspace-write`
- `danger-full-access`

The documented approval policies are:

- `untrusted`
- `on-request`
- `never`

The important CLI flag for Elanus is:

```sh
codex --dangerously-bypass-approvals-and-sandbox
```

The Codex manual explicitly says this should only be used inside an externally
hardened environment. That maps cleanly to Elanus if, and only if, the Elanus
cage is the real sandbox boundary for the whole Codex process tree.

Alternative: use Codex permission profiles to mirror the Elanus cage. That
would preserve Codex's native permission UI, but it creates two policy sources
that must stay in sync. Prefer Elanus as the single authority unless bypassing
Codex's sandbox proves incompatible with the TUI or approvals.

### Codex hooks

Hooks are enabled by default unless disabled in Codex config.

Useful hook events for Elanus:

- `SessionStart`
- `UserPromptSubmit`
- `PreToolUse`
- `PermissionRequest`
- `PostToolUse`
- `PreCompact`
- `PostCompact`
- `SubagentStart`
- `SubagentStop`
- `Stop`

`PreToolUse`, `PermissionRequest`, and `PostToolUse` can match tool names such
as `Bash`, `apply_patch`, `Edit`, `Write`, and MCP tool names. This is enough
for a coarse ledger of shell, edit, and MCP activity.

Current limitations:

- only `type = "command"` hooks run today
- `prompt`, `agent`, and async command handlers are parsed but skipped
- multiple matching command hooks run concurrently
- non-managed command hooks require Codex trust review
- `--dangerously-bypass-hook-trust` can bypass trust for one invocation, but
  only makes sense if Elanus generated or vetted the hook config

For Elanus, the first prototype should generate a temporary Codex config or
hook layer that calls a small `elanus emit` helper. That helper publishes the
hook payload to MQTT with an Elanus session id, timestamp, hook event, tool
name, and whatever structured payload Codex provides.

### Network / MQTT

If Codex's own sandbox remains active, Codex's local/private-network guard can
block local MQTT unless `localhost` or `127.0.0.1` is explicitly allowed in the
active Codex network profile. If Elanus bypasses Codex sandboxing and runs the
whole Codex process inside the Elanus cage, Elanus must allow the MQTT broker
connection itself.

### App-server note

Codex also documents `codex app-server`, `codex --remote`, and an experimental
`codex debug app-server send-message-v2`. Those are useful research paths for
message injection, but they are not the same as launching the normal standalone
TUI. They should not be the default design unless the real TUI cannot accept
inbound work through terminal supervision or another supported path.

## Recommended Launch Shape

`elanus codex [codex args...]` should:

1. Create an Elanus session id and ledger stream.
2. Resolve the target workdir and cage policy.
3. Generate an Elanus-owned Codex hook config for this invocation.
4. Launch the real `codex` binary inside the Elanus sandbox/cage.
5. Pass through the user's Codex CLI args.
6. Add Codex flags only where Elanus owns the replacement boundary:

```sh
codex \
  --cd <workdir> \
  --dangerously-bypass-approvals-and-sandbox \
  --dangerously-bypass-hook-trust \
  <user codex args...>
```

`--dangerously-bypass-hook-trust` should only be used for hooks generated by
Elanus. If user/project hooks are also loaded, Elanus should either require
normal Codex hook trust or isolate the generated hook layer from user hook
sources.

The Elanus cage should replicate the Codex posture the user intended:

- read-only cage for inspect-only work
- workspace-write cage for normal project work
- explicit extra writable roots when requested
- explicit network policy, including MQTT broker access
- deny rules for credentials and generated secrets where possible

Codex should still receive enough configuration to understand the operating
mode. Even when its sandbox is bypassed, `elanus codex` should pass a profile,
environment, or startup prompt that tells Codex it is running under Elanus
supervision and that approval/sandbox events are mediated by Elanus.

## Likely Architecture

`elanus codex` launches a terminal/session under Elanus supervision and starts
Codex inside it.

The session should be tied to:

- an agent profile
- a workdir
- a sandbox/cage policy
- a session id
- a ledger/history stream

MQTT should be the integration plane. If Codex can emit hooks, lifecycle events,
or structured activity, those events should be published over MQTT. If Elanus
needs to deliver work to this Codex session, it should publish into the session's
mailbox and Codex should pick it up at the next safe opportunity.

Hooks should be inserted deeply enough that Codex is "chatting over MQTT" for
the things it already does:

- before session start
- before command execution
- after command execution
- before file write
- after file write
- before git operation
- after git operation
- session end
- message received for this Codex session
- message accepted/queued for next opportunity

The point is not to trap Codex inside a fake UI. The point is to let Elanus own
the operating envelope: where it can work, what it can write, what gets recorded,
what needs approval, and what signals should be raised.

## Ledger Model

Use the minimum useful event model: publish whatever Codex activity is already
available over MQTT, with timestamps so reordering can be reconstructed.

Do not add extra model work just to make nicer logs. For example, do not ask for
reasoning summaries unless the user or Codex already requested them. Record
what exists:

- session started
- user input/message
- external MQTT message delivered or queued
- command started/finished
- file write attempted/completed
- git operation started/finished
- tool/hook result
- approval requested/accepted/declined
- session ended

The ledger should be good enough to reconstruct what happened, not a second
conversation product.

## Setup UI Implications

A future capability card should show:

- what gets launched
- which profile owns it
- which directory it can touch
- whether network/model credentials are needed
- what hooks are active
- what will be recorded
- how to stop/disable it

Risk badges should be explicit:

- writes files
- runs shell commands
- touches git
- may use network/model credentials
- records terminal session
- overrides Codex sandbox with Elanus sandbox
- accepts MQTT-delivered work

## Settled For Now

- Elanus launches Codex through `elanus codex`.
- The launched thing is the real Codex TUI.
- Elanus sandboxing is the authority boundary.
- MQTT is the preferred integration/event plane.
- Terminal/session events should be recorded opportunistically, not
  over-modeled.
- Codex's documented sandbox bypass flag is viable only when the process is
  already inside the Elanus cage.
- Codex hooks are viable for coarse ledger events, but not sufficient by
  themselves for full terminal capture or guaranteed inbound message delivery.

## Open Questions / Research

- Can Elanus inject a temporary Codex config/hook layer without polluting the
  user's `~/.codex` state?
- What exact JSON payload does each Codex hook receive, and does it include
  enough command/edit metadata for the ledger?
- Can `--dangerously-bypass-hook-trust` be scoped safely to Elanus-generated
  hooks, or does loading user/project hooks make that too broad?
- How should a running real Codex TUI notice MQTT-delivered messages without
  interrupting an unsafe moment?
- Is terminal supervision enough for inbound messages, or does this require
  the experimental app-server protocol?
- How should Codex `PermissionRequest` hooks map to Elanus approvals when Codex
  approvals are bypassed?
- Does the existing agent cage fully cover the Codex process tree, PTY, hooks,
  spawned commands, temp files, and network access?
- How close must Elanus get to Codex permission-profile semantics for Codex to
  remain predictable to experienced Codex users?
