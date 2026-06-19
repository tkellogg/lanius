# Handoff: coding-agent support (Codex & Claude Code)

Status: planned, not started. Intended implementer: Codex (with Tim, and possibly
a planner agent, in the loop).

This handoff asks elanus to **launch and supervise external coding agents** — the
Codex CLI/TUI and Claude Code — and bridge their lifecycle, tools, and context
into elanus's bus, sandbox, recording, and context substrate. The end state is
not "a nicer terminal for one coding agent." It is the operating envelope that
lets a big-picture planner agent and a cheap detail-worker coding agent
collaborate, with elanus as the room they work in.

Treat Codex and Claude Code as **one envelope, two adapters.** To elanus both are
an external actor brought up from the command line (docs/actors.md). Do not design
two integrations; design the envelope once and adapt the two tools' hook/config
surfaces to it.

## The vision (why this is worth doing)

What Tim does by hand today: a planner model writes a milestone, Tim pastes it
into Codex, waits, reads the result, carries it back to the planner to review,
then prompts the next milestone. Tim is the message bus. The goal is to close that
loop on elanus:

1. **Planner drives worker.** A planning agent publishes work into a coding
   session's mailbox, the session works and reports completion, the planner
   reviews and issues the next milestone — the same addressed-message machinery
   every actor already uses, no special "drive Codex" tool.
2. **Sessions coordinate.** Run more than one coding session (e.g. one writing,
   one verifying) and let them coordinate over the bus — advisory, not locking:
   "I'm editing src/foo.rs, ping me if you go near it." A room is just a
   ledger-backed noun with a mailbox (docs/topics.md).
3. **Context is ambient.** A hook on the coding agent asks elanus each turn what
   this session should know — open inbox messages, current edit claims, a memory
   block the planner left — and folds it into the prompt. Collaboration becomes
   context the agent reads, not a protocol it speaks.

The narrative for all four characters is docs/journeys/02-claude-code.md (Tim and
Daniel primary). Read it first for the *why*.

## Read these first

Design intent and substrate:

- `docs/journeys/02-claude-code.md` — the unified coding-agents journey, covering
  Codex and Claude Code as one experience. The why. (There is deliberately **one**
  coding-agent journey: the user experience is identical, only the technical
  adapters differ. The adapter references are Appendices A and B of this handoff.)
- `docs/actors.md` — coding agents are external actors launched from the CLI; the
  launcher is not the actor; every actor has a mailbox. Crucially: the
  **egress/provenance lesson** at the end — a daemon bridge carries its own token
  so the broker stamps its records correctly; an uncaged exec handler authenticates
  as the owner and mislabels its sends (docs/security.md entry 16). The coding
  session bridge must carry its own minted identity.
- `docs/context.md` — the context-program substrate. Blocks carry a `Placement`
  (system-text vs **user-text**), registers, computed blocks, build log. This is
  the machinery behind milestone M3 (memory blocks injected through the coding
  agent's prompt hook). The `recent-history` resident stage is the exemplar of a
  computed cross-run context source.
- `docs/bus.md` — planes (observation / work / hook), the hook plane (exec and
  resident), `elanus emit`, the recorder, supervisor-minted per-spawn actor
  tokens, and the **KNOWN GAP** (loopback bus is unauthenticated = "the human";
  exec handlers run uncaged with `ELANUS_DB`). A caged coding agent can still reach
  the loopback bus — that's wanted here, but read the gap before you lean on the
  ACL as a boundary.
- `docs/topics.md` — the verb-first grammar. Mailboxes `in/agent/<name>/<conv>`,
  telemetry `obs/agent/<name>/<session>/...`, rooms `in/group/<id>`
  (ledger-backed, late joiners read history), `el-correlation` user property.
- `docs/sandbox.md` — the cage/camera split, grants, leases, fs events; the
  authority boundary the coding agent runs inside.
- `docs/security.md` — read before claiming any containment property; entries
  13–16 and the identity gaps are directly relevant to a process that holds
  credentials and talks to the bus.

Likely implementation anchors (confirm before building — this is where the seams
are, not a prescription):

- `src/exec.rs`, `src/sandbox.rs` — how runs are spawned inside the cage; where an
  `elanus code` launcher would put a coding-agent process tree.
- `src/hooks.rs`, `src/dispatcher.rs`, `src/broker.rs`, `src/resident.rs` — the
  hook plane and dispatch; how `elanus emit` publishes; how observations are
  recorded and announced.
- `src/secrets.rs`, `src/paths.rs` — per-spawn token minting and fenced state, for
  the session's own identity.
- `src/context.rs`, `src/context_blocks.rs`, `src/render.rs`,
  `packages/recent-history/` — the block/context substrate the M3 prompt hook
  reads from.
- `packages/webhook/` — the **daemon-bridge exemplar** (carries its own token,
  POSTs directly, emits `obs/channel/<kind>/sent`). Copy this shape for any
  long-lived bridge, not the uncaged exec-handler shape.
- `ui/web/src/App.tsx`, `ui/web/server.mjs` — where a coding-session capability
  card and live view would surface (later milestones).

## Architecture (decisions and the tensions to resolve)

These are the load-bearing choices. Some are settled by the existing docs; some
are genuine open questions flagged honestly for you to resolve against the real
tools, not guess.

### A coding agent is an external actor with two operating modes

The key clarification the research forces: there is **no supported way to type
into a running interactive TUI** (neither Codex nor Claude Code exposes
"inject a message into the live session"). Programmatic drive is headless: a fresh
turn resumed into the same session (`claude -p --resume <id>`, Codex's equivalent),
or stream-json/SDK. So the envelope has **two modes**, and they are different
products:

- **Supervised-interactive.** A human sits at the real TUI. elanus owns the cage
  and *observes* (hooks → bus) and *injects context* (the prompt hook), but does
  not drive turns. Because elanus cannot push a turn into the live session, the
  delivery mechanism here is an **inbox**: messages addressed to the session
  accumulate, and the agent *checks the inbox* when it chooses — prompted by the
  per-turn injection ("you have N new messages"). This is Daniel's mode and the
  honest reading of "launch the real TUI."
- **Headless-orchestrated.** elanus (or a planner agent) drives the session
  programmatically — headless turns resumed into one session id (`claude -p
  --resume`, `codex exec` resume), or the Agent SDK. A delivered message drains the
  inbox by triggering a resumed turn. This is the mode the orchestration vision
  (planner→worker, multi-session) needs. The "real TUI" is not in this loop; the
  real *session* and *model loop* are. The hard part here is **keeping the human in
  the loop** — they aren't watching a TUI, so the hook→bus record and the elanus UI
  are how they see what the headless turns did.

Design the envelope so a session can be either, over the same inbox: interactive
sessions *pull*, headless sessions are *driven*. Do not promise interactive
message-injection into a live TUI; it isn't there.

### The hook→bus bridge is the ledger

elanus generates a **scoped hook config** for each launch whose hooks call a small
helper (`elanus emit` or `elanus code hook`) that publishes the hook payload to
`obs/agent/<name>/<session>/...` with the elanus session id and a timestamp.
Coarse but ordered: session start, user message, tool/command pre+post, file
write, git op, stop. This is enough to reconstruct what happened (docs/bus.md
recorder + the existing `obs/agent/<name>/<sess>/tool/<name>/{call,result}`
grammar).

- **Claude Code:** configure via `--settings <file|json>` plus `--bare` /
  `--setting-sources` so the generated hooks load and the user's `~/.claude` is
  **not** polluted (Appendix A). PreToolUse can also mediate approvals
  (`permissionDecision`).
- **Codex:** generate a temporary Codex config/hook layer; `type=command` hooks
  only, today (Appendix B). `--dangerously-bypass-hook-trust` is only acceptable
  for elanus-generated hooks.

### Scoped config, no pollution; the cage is the authority

Launch the real binary inside the elanus cage. The coding agent's own sandbox is
either bypassed (`codex --dangerously-bypass-approvals-and-sandbox`,
`claude --dangerously-skip-permissions`) **only because the elanus cage is the
real wall**, or mirrored — prefer elanus as the single authority (Appendix B).
Generated config lives in a temp/isolated location, never the user's home state.
Confirm the cage actually covers the whole process tree, PTY, spawned commands,
temp files, and network (Appendix B open question; this is the property the whole
safety story rests on).

### Provenance: the session carries its own identity

A coding session that publishes to the bus must do so as **itself**, with a
supervisor-minted per-session token (the pattern script actors already use,
docs/bus.md migration note d), so the broker stamps its observations and any work
it emits with a genuine sender — not as the owner. This is the actors.md / entry-16
lesson; getting it wrong means a coding agent's actions are misattributed, which
poisons both the record and any cross-agent trust. Mint identity per session.

### Context injection: the system-reminder seam

The M3 seam is the coding agent's per-turn prompt hook (`UserPromptSubmit`)
calling back into elanus, which returns context assembled from the block substrate
(docs/context.md) — approved blocks for this session plus computed blocks like
inbox status and current edit claims.

Inject it as an **out-of-band system note — not as the user speaking, and not in
the cached system prefix.** This is both cache-friendly (it lands after the stable
cached prefix, so per-turn changes don't bust the prompt cache) and *more honest
than the user prompt*: an inbox status or a coordination claim genuinely is a
reminder from the system, not the user. Claude Code does exactly this natively —
`UserPromptSubmit` and `SessionStart` `additionalContext` land in the
**system-reminder layer** (Appendix A). This is better than the original
"put memory in the user prompt for caching" idea, not a compromise of it: same
caching benefit, truer semantics. The system-reminder layer is not a distinct API
field — it's an application convention (text the harness tags as an out-of-band
system note and places after the cached prefix, so the model reads it as a system
reminder rather than as the user).

So the action is **parity, not a workaround**: for the Codex/OpenAI adapter, find
the equivalent out-of-band channel and prefer it. OpenAI's chat/responses APIs
have `system`/`developer` roles; the analog is injecting a `developer` (or
`system`) note into the turn rather than into the user message. **Confirm where
Codex's own hook injection actually lands (developer/system message vs user text)
and route through the out-of-band channel for parity.** Measure cache hit/miss
either way (docs/context.md already wants cacheability as a signal) and record what
each tool does.

## Milestones

Ordered smallest-useful-demo → orchestration. Each is shippable alone; M0–M1 are
the demo Daniel needs, M2–M3 unlock supervision and the context seam, M4–M5 are
the orchestration vision. "Shape" is intent + constraints; the launcher surface,
adapter details, and UI are yours to design within the guardrails.

### M0 — Launch inside the cage (smallest demo)

Shape:
- A launcher (`elanus code <tool> [args]`, or `elanus codex` / `elanus claude`)
  that starts the real binary in a resolved workdir inside the elanus cage, with a
  per-session elanus session id and a minted session token. No event bridge, no
  injection, no inbound delivery yet.
- Generated config goes to an isolated location; the user's `~/.codex` / `~/.claude`
  is untouched.

Acceptance criteria:
- Running the launcher in a project starts a normal, fully usable coding session
  (real TUI for the interactive mode).
- A write outside the workdir/approved prefixes is denied by the cage (prove it —
  attempt a write to a path outside and show it fails).
- A new elanus session id exists for the run; the user's coding-agent home state is
  unchanged after exit (diff it).

### M1 — Hook→bus event bridge (the record)

Shape:
- Generate a scoped hook config per launch that routes documented hook events
  through `elanus emit` to `obs/agent/<name>/<session>/...`: session start, user
  message, tool/command pre+post, file write, git op, stop. Both adapters.
- Timestamps and the session id on every event so order reconstructs. No
  `~/.codex`/`~/.claude` pollution (CC: `--settings` + `--bare`/`--setting-sources`;
  Codex: temp config + bypass-hook-trust scoped to generated hooks only).

Acceptance criteria:
- A coding session's shell commands, edits, and git operations appear as ordered
  observations on the bus tied to the elanus session, viewable in the elanus UI's
  telemetry/signals.
- The sequence is sufficient to reconstruct what the session did (commands + edits
  + git, in order).
- The session's observations are stamped with the session's own identity, not the
  owner (verify the sender).
- Confirm and record the exact hook JSON payloads each tool delivers (Appendix B
  open question for Codex; Appendix A for CC).

### M2 — Inbound delivery: the session inbox

Shape:
- Every coding session has an **inbox**: its mailbox (`in/agent/<name>/...`)
  accumulates messages addressed to it. Drain it two ways, by mode:
  - **Interactive (pull):** elanus cannot inject a turn, so the inbox is surfaced
    passively — the M3 system-reminder injection shows inbox status each turn
    ("2 new messages — read them with <inbox affordance>"), and a small read/ack
    affordance (a tool, or an `elanus inbox` command the agent can run) lets the
    agent pull and act when it chooses. The human keeps the TUI; elanus just makes
    the inbox visible and pullable.
  - **Headless (driven):** a delivered message triggers a resumed turn (`claude -p
    --resume <id>`, `codex exec` resume, or the SDK) that drains the inbox; results
    return as observations. Because the human isn't watching a TUI, the hook→bus
    record and the elanus UI are how they stay in the loop on what happened.

Acceptance criteria:
- Interactive: a message sent to a live session shows up as inbox status in the
  agent's next-turn context, and the agent can pull and act on it via the read
  affordance; the exchange is observed on the bus, threaded by correlation.
- Headless: a message sent to a session triggers a resumed turn into the **same
  session id** (history continuity is real, not a new session each time), acts on
  it, and returns the result as observations.
- Deliveries and results are recorded; a human can reconstruct the exchange from
  the elanus UI without having watched the session.

### M3 — Memory/inbox/context via the prompt hook

Shape:
- Wire the coding agent's per-turn prompt hook to elanus's block substrate: return
  approved blocks for this session plus computed blocks — an "open inbox" status
  and (after M5) "current edit claims." Inject through the out-of-band
  system-reminder seam (see "Context injection" above), after the cached prefix.
- A block changed in elanus changes what the next turn sees.

Acceptance criteria:
- A configured memory block (e.g. a project brief, or inbox status) appears in the
  coding agent's context each turn, sourced from elanus blocks.
- Editing the block in elanus changes the injected context on the next turn
  (prove by reload + a second turn).
- Cache behavior is measured and recorded; the chosen seam meets the
  "don't bust the cached prefix" goal, and the doc states where injected context
  actually lands for each tool.

### M4 — Orchestration loop: planner drives worker

Shape:
- A planner agent launches/prompts a coding session for a milestone, detects
  completion (the Stop/idle event from M1), reviews the result, and issues the next
  milestone — gated on the worker's done-signal. Provide the supervision primitive
  (session lifecycle event → "ready for next work") and a minimal planner recipe.
- The human can watch the whole loop on the bus and interrupt.

Acceptance criteria:
- A planner runs at least two milestones of some real task by launching/prompting a
  coding session, each next step gated on the prior worker completion event — with
  no human acting as the wire.
- Every handoff (planner→worker prompt, worker→planner completion) is observable on
  the bus; the human can interrupt mid-loop.
- A worker failure surfaces (signal/observation) rather than hanging the loop.

### M5 — Peer coordination over the bus (advisory)

Shape:
- Multiple concurrent coding sessions share a coordination room (`in/group/<id>`,
  ledger-backed). A session announces an edit claim ("editing src/foo.rs"); each
  session's M3 hook injects the current claims into its prompt. Advisory only — no
  hard locks.

Acceptance criteria:
- Two concurrent sessions: one announces a file claim, the other's injected context
  reflects it within its next turn.
- In a scripted scenario the second session routes around the claimed file rather
  than colliding (demonstrated, with the bus record showing the claim was seen).
- A session that exits releases its claims (lease-style, crash-released — the room
  membership lease in docs/topics.md decided-5).

## Guardrails

- **One envelope, two adapters.** Shared launch/cage/record/mailbox/context core;
  Codex- and CC-specific surfaces isolated to thin adapters.
- **Don't reimplement or fake the tool.** The launched thing is the real coding
  agent. No fake web UI around it (journey 05).
- **elanus is the authority boundary.** Bypass flags are valid only inside the
  cage; confirm the cage covers the full process tree/PTY/network before relying on
  it. Fail closed where a missing boundary would leak.
- **No home-state pollution.** Generated config/hooks live in isolated locations;
  the user's `~/.codex` / `~/.claude` is never edited.
- **Provenance is real.** Per-session minted identity; the broker stamps the
  sender. Never let a session authenticate as the owner.
- **Record what exists, don't over-model** (journey 05 ledger model): publish the
  activity the tool already exposes, with timestamps; don't add model work just for
  nicer logs.
- **Layering rule** for any product surface (a coding-session capability card,
  risk badges): no internal vocabulary; translate at the boundary (docs/layering.md).
- **Verify against the real tools, not memory.** Hook payload shapes, injection
  placement, cage coverage, and inbound-delivery mechanics are all things to
  confirm live and record in the Log below — several are open questions, not facts.

## Non-goals

- A general multi-coding-agent product UI beyond what a milestone needs.
- Replacing the coding agents' own model/provider config — they own that
  (journey 02 open question; let the tool own its model unless a milestone needs
  otherwise).
- Closing the docs/bus.md identity/containment gap (unauthenticated loopback = the
  human). This handoff must not *depend* on the bus ACL as a security boundary
  against hostile code; note where it relies on cooperative behavior and leave the
  gap to its own pass.
- Real billing — cost stays honest labels (run-step/spend ceilings), per
  docs/journeys/03-cost-visibility.md.

## Appendix A — Claude Code reference (confirmed 2026-06-19)

Authoritative facts gathered for this handoff (flag-level; re-verify versions when
building, several are version-dependent):

- **Hook events (large set).** Session: `SessionStart`, `Setup`, `SessionEnd`.
  Per-turn: `UserPromptSubmit` (timeout 30s), `Stop`, `StopFailure`. Tool loop:
  `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionRequest`,
  `PermissionDenied`. Subagent/task/file: `SubagentStart`, `SubagentStop`,
  `TaskCreated`, `TaskCompleted`, `FileChanged`, `CwdChanged`. Context:
  `PreCompact`, `PostCompact`. Plus `Notification` (fires when waiting for
  input/permission). Source: code.claude.com/docs/en/hooks.
- **Context injection.** `SessionStart` and `UserPromptSubmit` inject stdout text
  (exit 0) as `additionalContext`; JSON form:
  `{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"…"}}`.
  **Placement: the system-reminder layer, NOT the user-message body.** Putting text
  literally in the user message requires the SDK / controlling the actual input,
  not a hook. This is the desired channel (see "Context injection: the
  system-reminder seam") — out-of-band, after the cached prefix, semantically a
  system note. The system-reminder layer is an application convention, not a raw
  API field.
- **PreToolUse decision.**
  `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow|deny|ask|defer","permissionDecisionReason":"…","updatedInput":{…}}}`.
  `allow` skips the prompt but cannot override settings deny rules; `deny` cancels
  and feeds the reason back; `defer` is `-p`-mode only (preserves the tool for the
  SDK to resume). `PermissionRequest` hooks do **not** fire in `-p` mode — use
  `PreToolUse` for automated decisions.
- **Scoped config without polluting `~/.claude`.** `--settings <file|json>` loads an
  explicit settings object; `--bare` skips auto-discovery (hooks/skills/MCP/
  CLAUDE.md); `--setting-sources project,local` (etc.) limits which scopes load.
  Combine `--bare` + `--settings <generated>` to run *only* generated hooks. Hook
  stdin payload includes `session_id`, `transcript_path`, `cwd`, `permission_mode`,
  `hook_event_name`, plus event-specific fields (`tool_name`, `tool_input`,
  `prompt`, …).
- **Headless / programmatic drive.** `-p/--print`; `--output-format
  json|stream-json|text` (+ `--include-partial-messages`, `--include-hook-events`,
  `--verbose`); `--input-format stream-json` (newline-delimited JSON on stdin,
  capped ~10 MB); `--append-system-prompt[-file]`; `--continue`, `--resume <id>`,
  `--fork-session`, `--session-id <uuid>`. **No native inject-into-running-session**
  — a new message is a fresh turn resumed into the same session id. The Agent SDK
  (TS/Python) is the richer programmatic alternative (message objects, tool-approval
  callbacks).
- **MCP + permissions.** `--mcp-config <file|json>`, `--strict-mcp-config` (load
  only the given MCP config). `--permission-mode default|acceptEdits|plan|auto|
  dontAsk|bypassPermissions`; `--dangerously-skip-permissions` allows tools without
  prompts but does **not** bypass `PreToolUse` hook denies or settings/managed deny
  rules.

## Appendix B — Codex reference

The Codex adapter reference (formerly the standalone `05-codex-integration.md`
journey, folded in here so there is a single coding-agents journey). Re-verify
against the current Codex manual when building — Codex is evolving and several of
these are version-dependent.

- **Sandbox modes:** `read-only`, `workspace-write`, `danger-full-access`.
  **Approval policies:** `untrusted`, `on-request`, `never`. Sandbox is separate
  from approval policy (sandbox = the technical boundary; approval = when Codex
  pauses before crossing it). Enforcement is platform-native: macOS Seatbelt;
  Linux/WSL2 `bubblewrap` (with a bundled helper fallback). It applies to **commands
  Codex spawns** — shell, `git`, package managers, test runners — not only built-in
  edits.
- **Launch flags:** `codex --dangerously-bypass-approvals-and-sandbox` — the Codex
  manual says use it **only inside an externally hardened environment**, which maps
  to elanus only if the elanus cage is the real boundary for the whole Codex process
  tree. `--cd <workdir>`. `--dangerously-bypass-hook-trust` — one-invocation hook
  trust bypass, acceptable **only for elanus-generated hooks**. Alternative to
  bypass: mirror the cage with Codex permission profiles (keeps Codex's native
  permission UI but creates two policy sources to keep in sync — prefer elanus as
  the single authority unless bypass proves incompatible with the TUI).
- **Hooks:** enabled by default unless disabled in config. Useful events:
  `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`,
  `PostToolUse`, `PreCompact`, `PostCompact`, `SubagentStart`, `SubagentStop`,
  `Stop`. `PreToolUse`/`PermissionRequest`/`PostToolUse` match tool names (`Bash`,
  `apply_patch`, `Edit`, `Write`, MCP tool names) — enough for a coarse ledger.
  Limits today: only `type = "command"` hooks run (`prompt`/`agent`/async parsed but
  skipped); multiple matching command hooks run concurrently; non-managed hooks need
  Codex trust review. The bridge: generate a temporary config/hook layer that calls
  a small `elanus emit` helper publishing the hook payload to MQTT with the elanus
  session id, timestamp, hook event, tool name, and structured payload.
- **Network / MQTT:** if Codex's own sandbox stays active, its local/private-network
  guard can block local MQTT unless `localhost`/`127.0.0.1` is allowed in the active
  network profile. If elanus bypasses the Codex sandbox and runs Codex inside the
  elanus cage, elanus must allow the broker connection itself.
- **Inbound / app-server:** Codex documents `codex app-server`, `codex --remote`,
  and experimental `codex debug app-server send-message-v2` — research paths for
  message injection, distinct from launching the normal standalone TUI. Not the
  default design unless the real TUI can't accept inbound work through terminal
  supervision or another supported path. (This is the Codex side of the "no native
  inject-into-running-TUI" reality — the inbox/headless-resume model in M2 is the
  fallback.)
- **Context-injection placement (verify):** confirm whether Codex's
  `UserPromptSubmit` hook injection lands as a developer/system message or as user
  text, and prefer the out-of-band channel for parity with CC's system-reminder
  layer (see "Context injection" above). OpenAI's chat/responses APIs have
  `system`/`developer` roles to target.
- **Open questions (this handoff's too):** exact JSON payload of each Codex hook;
  whether `--dangerously-bypass-hook-trust` can be scoped safely to generated hooks
  when user/project hooks also load; inbound delivery into a running TUI (terminal
  supervision vs app-server); whether the cage covers the full Codex process tree,
  PTY, spawned commands, temp files, and network; how `PermissionRequest` maps to
  elanus approvals when Codex approvals are bypassed.

## Log (fill in as you build)

- Launcher surface chosen (`elanus code` vs per-tool) and why: _TODO_
- Cage coverage of the coding-agent process tree / PTY / network — verified?: _TODO_
- Exact hook JSON payloads (Codex; CC event-specific fields): _TODO_
- Context-injection placement per tool + measured cache behavior (prefer the
  out-of-band system-reminder/developer channel; confirm Codex placement): _TODO_
- Inbound-delivery mechanism actually used (headless resume vs SDK vs stream-json):
  _TODO_
- Per-session identity minting path: _TODO_
