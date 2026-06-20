# Handoff: coding-agent support (Codex & Claude Code)

Status: **M0+M1 landed for BOTH adapters; M2-A (durable resumable sessions + the
resume primitive) landed 2026-06-20; M2-B (daemon-driven inbound delivery off a
session mailbox → resume) landed 2026-06-20** — Claude Code (hook bridge)
2026-06-19, Codex (`codex exec --json` stdout stream) 2026-06-20, branch
`coding-agents`. All verified end to end against the live worktree stack. **M4 (the
planner side — an agent sending work + waking on the completion obs) and M3
(interactive pull / a session read grant) are next/deferred; M5 is not started.**
See the Log at the bottom for the as-built decisions (the two adapters share one
envelope but differ in their *capture mechanism* — CC's hooks vs Codex's JSONL
stream; the durable-session split-record model is in the M2-A entry; the
mailbox→resume drive + per-session serialization are in the M2-B entry).

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

### Scoped config, no pollution

Launch the real binary in a resolved workdir, with generated config (hooks,
sandbox/permission settings) in a temp/isolated location — never the user's
`~/.codex` / `~/.claude` home state.

### Sandboxing: one cage, not two

Both coding agents sandbox, and so does elanus — and on the platforms that matter
they use the **same primitive**: macOS Seatbelt (`sandbox-exec` — docs/sandbox.md
notes this is exactly what Claude Code's own sandbox uses), Linux bubblewrap +
Landlock. So "two layers" is not two technologies; it is the same mechanism
applied twice. That settles the shape:

- **Do not run two independent OS sandboxes** (the naive "both"). Nesting the same
  primitive is fragile and only ever *intersects*: an inner Seatbelt/bubblewrap can
  fail to initialize inside the outer one (nested user namespaces especially), or
  it forbids something the outer cage meant to allow — confusing EPERMs — and it is
  two policy sources to keep aligned. Bad default.
- **Layer by concern instead (the good "both").** elanus owns the OS **cage** over
  the whole process tree (fail-closed, inherited across fork/exec, so everything the
  agent spawns is covered — docs/sandbox.md). The tool keeps its **approval policy**
  (permission prompts / on-request / untrusted) as a cooperative *inner UX* layer —
  approval is separate from the sandbox in both Codex and CC, so you keep the
  familiar prompts without a second OS wall.
- **Single authority = reconstruct the posture in the cage (preferred).** Bypass the
  tool's redundant OS sandbox (`--dangerously-bypass-approvals-and-sandbox` /
  `--dangerously-skip-permissions`) and make the elanus cage the one wall, mapping
  the tool's posture modes onto cage settings: Codex `read-only` / `workspace-write`
  / `danger-full-access` (and CC permission modes / allowed-tools) ↔ the cage's write
  set, read scope, and network policy. Replicate enough of the tool's expected
  semantics that it still behaves (workspace writes succeed, `/tmp` writable, network
  limited-not-absent where the mode expects it).

**The prerequisite — a complete cage, built not staged.** This works only when the
elanus cage actually does what the tool's sandbox did: restrict **reads and
network**, not just writes. Today the cage is a write-fence (reads/network open);
docs/sandbox.md [DECIDED 2026-06-19] promotes read scoping + egress from "deferred"
to the single-cage **end state**, precisely because this envelope needs them and
the project is pre-release (no migration reason to stage). So treat the complete
cage (write + read + egress) as a **core-elanus prerequisite** of the bypass: build
it, reconstruct the full tool posture onto it, and only then bypass the tool's own
sandbox. Do not ship a staged half-measure that bypasses onto a write-only fence;
if the cage's read/egress work isn't done yet, that's a blocking dependency to
close in core, not a reason to keep the tool's sandbox as a permanent crutch.

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
- **Sandbox posture: the single cage.** Bypass the tool's own sandbox onto the
  elanus cage, which does write + read + egress scoping (the end state in
  docs/sandbox.md), and reconstruct the tool's posture modes onto it. That complete
  cage is a core-elanus prerequisite of this milestone — build it, don't stage
  around it with the tool's own sandbox.

Acceptance criteria:
- Running the launcher in a project starts a normal, fully usable coding session
  (real TUI for the interactive mode).
- A write outside the workdir/approved prefixes is denied by the cage (prove it —
  attempt a write to a path outside and show it fails).
- A read of a sensitive path outside the agent's read scope is denied by the cage,
  and network egress outside policy is blocked — reads and network are contained,
  matching what the tool's own sandbox would have done (prove both, not just writes).
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
  agent. No fake web UI around it (journey 02).
- **One OS cage, and a complete one.** The elanus cage must restrict writes, reads,
  and network (docs/sandbox.md end state) before it replaces the tool's own sandbox —
  bypassing onto a write-only fence is a containment regression, so the complete cage
  is a prerequisite, not an afterthought. One OS cage, never two; keep the tool's
  *approval* UX as a cooperative inner layer. Fail closed where a missing boundary
  would leak.
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

### 2026-06-19 — M0 launcher + M1 hook→bus bridge, Claude Code adapter (branch `coding-agents`)

- **Launcher surface chosen — `elanus code <tool> [args...]`, not per-tool.**
  Rationale: "one envelope, two adapters" (the guardrail). The `<tool>` positional
  selects the adapter (`claude` today; `codex` errors with "next increment"); a
  shared launch/identity/record core lives in `src/codeagent.rs`, only the
  tool-specific surface (binary name, settings shape, hook-event map) is
  adapter-local. A hidden sibling `elanus code hook <Event>` is the bridge the
  generated hooks invoke; not meant to be run by hand. CLI wired in `src/main.rs`
  as a flat `Cmd::Code { tool, args }` — `elanus code <tool> [args...]` launches;
  the reserved first word `hook` (`elanus code hook <Event>`) is the bridge. (The
  fix pass flattened an earlier `code launch <tool>` subcommand to this documented
  `code <tool>` form so the CLI and the docs agree.) Module `src/codeagent.rs`.

- **Per-session identity minting path — a GRANT-SCOPED session token** (revised in
  the fix pass; see the entry below). Each launch mints session id `code-<8hex>` and
  writes a 0600 **scoped token** into the fenced store at
  `Root::secrets()/code-sessions/<session>.json` (src/codesession.rs). The launcher
  is uncaged (the human ran it) so it can place it; a caged agent cannot read the
  fenced store, so it can never forge a session identity — the same asymmetry as
  before. The launcher passes `ELANUS_PACKAGE=<principal>` +
  `ELANUS_BUS_TOKEN=<secret>` to the child, so every observation the bridge
  publishes is **broker-verified and stamped `sender = code-<session>` — never the
  owner** (docs/actors.md egress lesson / security entry 16). The token is retired
  when the session ends and reaped at launcher/daemon boot if a SIGKILL leaked it.
  **Crucially, the broker resolves a `code-*` principal as a grant-scoped actor
  (`actor = Some`), NOT a full-authority fenced secret (`actor = None`)** — so the
  bus ACL gates run and the session is held to its own `obs/agent/<agent>/<session>/#`
  subtree. (The original slice minted a plain fenced secret here, which resolved as
  full authority and skipped every ACL gate — the high-severity gap the fix pass
  closed.)

- **Hook JSON payloads (Claude Code, confirmed live, CC 2.1.183).** Hook stdin
  carries `session_id`, `cwd`, `permission_mode`, `hook_event_name`, plus
  event-specific fields. Confirmed shapes: `PreToolUse` → `tool_name` + `tool_input`
  (object); `PostToolUse` → `tool_name` + `tool_input` + `tool_response` (object,
  e.g. `{stdout,stderr,interrupted,...}`); `UserPromptSubmit` → `prompt`;
  `SessionStart` → `source` ("startup"); `SessionEnd` → `reason` ("other").
  `map_event` in src/codeagent.rs maps these to obs leaves matching the exec.rs
  grammar: `session/{start,started,idle,stop}`, `user/message`,
  `tool/<name>/{call,result}`. A real headless run produced the full ordered
  record: session/start → session/started → user/message → tool/Bash/call →
  tool/Bash/result(failed:false) → session/idle(Stop) → session/idle(SessionEnd)
  → session/stop(exit_code:0). Sufficient to reconstruct the session.

- **Scoped config, no `~/.claude` pollution — VERIFIED.** Generated settings (hooks
  only, every command routing to `elanus -C <root> code hook <Event>`) live in
  `<root>/run/<session>/settings.json`. Launched with `--settings <file>` +
  `--setting-sources ''` (empty = load NO user/project/local settings, so the user's
  `~/.claude` hooks/CLAUDE.md auto-discovery are untouched; only the generated hooks
  run). After a real run, `~/.claude/settings.json` mtime was unchanged and no
  config file under `~/.claude` was modified; the generated scratch + session secret
  were cleaned up. (`--bare` was NOT used — it skips hooks entirely; the
  `--settings` + empty `--setting-sources` combo is the correct no-pollution way to
  run ONLY generated hooks.) **Scope of the no-pollution claim:** it is about
  *config/hooks* — the user's `~/.claude` settings, hooks, and CLAUDE.md
  auto-discovery are untouched. Claude Code still writes its own per-session
  **transcripts** under `~/.claude/projects/…` (its normal session history); that is
  the tool's own state, not elanus config, and is deliberately left alone. So the
  guarantee is "elanus changes no user config," not "the tool writes nothing to its
  home" (the latter would mean rewriting the tool).

- **Sandbox stance for this increment — the tool keeps its OWN sandbox active; NO
  bypass onto today's write-only cage (deliberate sequencing, recorded per the
  handoff guardrail).** Today's elanus cage (src/sandbox.rs) (a) enforces **writes
  only** on macOS Seatbelt — reads and network are open — and (b) is built for
  one-shot captured `sh -c` calls (stdin null, stdout piped, timeout-killed), NOT an
  interactive long-lived TUI with inherited stdio. Bypassing Claude Code's own
  sandbox onto that would be a containment regression (M0's read-denied/
  network-blocked acceptance criteria need the COMPLETE cage that docs/sandbox.md
  [DECIDED 2026-06-19] promotes to the end state but which is NOT built yet). So for
  now the launcher does NOT pass `--dangerously-skip-permissions`-style bypass by
  default; the tool's sandbox + approval UX stay active (reads/network contained),
  and elanus owns the workdir + observation + identity. The single complete cage
  (write + read + egress + the read camera) is a **core-elanus prerequisite**; the
  tool-sandbox bypass + posture reconstruction (M0's "single cage" criteria) is a
  LATER milestone gated on it. This is implementation sequencing, not product
  staging — and the honest blocker the handoff flagged. **Cage coverage of the
  coding-agent process tree / PTY / network: NOT verified, because the cage is not
  in the launch path yet (by the decision above).**

- **Tests + verification.** `cargo test` green (86 passing at the M0+M1 commit).
  Live end-to-end run through the worktree dev stack (root
  `~/.elanus/wt-coding-agents`, broker `:1893`) drove a real headless `claude -p`
  and captured the full ordered, correctly-attributed record on
  `obs/agent/claude-code/#`. (Superseded by the fix-pass numbers below.)

### 2026-06-19 — Fix pass: grant-scoped session token + reaper + CLI/adapter cleanup

An adversarial verifier confirmed a **high-severity authority gap** in the M0+M1
slice: the per-session `code-<session>` credential was a plain fenced secret, which
the broker resolves as a **full-authority principal** (`actor = None`) — and every
bus ACL gate is `if let Some(pkg) = &actor`, so *none* of them ran. A minted
session token could publish to `in/human/owner`, `work/agent/exec`, and other
agents' mailboxes, and subscribe `obs/#` (read every agent's telemetry). Attribution
was correct (`sender = code-<session>`, forge-resistant) but authority was
owner-equivalent, and a SIGKILL leaked the live credential. This pass closed it.

- **(HIGH, FIXED) Grant-scoped per-session actor token.** New module
  `src/codesession.rs` owns the session credential. It is minted into a fenced
  sub-store `Root::secrets()/code-sessions/<session>.json` (still cage-fenced — the
  forge-resistance asymmetry is unchanged: only the uncaged launcher can place it).
  The broker (`src/broker.rs` `handshake`) resolves a `code-*` principal **before**
  the fenced-secret path and as a **grant-scoped actor** (`actor = Some`), so every
  ACL gate runs. The scope is **structural**, not grant-table rows (a session has no
  manifest): `actor_may_publish`/`actor_may_subscribe` route `code-*` actors through
  `codesession`, which permits publish ONLY to the session's own
  `obs/agent/<agent>/<session>/#` and subscribe to nothing. No daemon-side
  `register_actor` RPC was needed — the structural scope rides the fenced token the
  launcher already writes, which the broker reads at CONNECT. This copies the webhook
  daemon's grant-scoped shape (own token, narrow filter) rather than inventing a new
  identity mechanism. **Proven live** (worktree stack, broker `:1893`) with a real
  `code-<session>` token: publish to `in/human/owner`, `work/agent/exec`, another
  agent's mailbox, and another session's obs all now PUBACK `NotAuthorized`;
  subscribe `obs/#` SUBACKs `NotAuthorized`; the session's own
  `obs/agent/claude-code/<session>/...` publish still succeeds and lands stamped
  `sender = code-<session>`. A real headless `claude -p` run produced the full
  ordered record unchanged.

- **(MED, FIXED) Reap orphaned `code-*` credentials.** `codesession::reap_orphans`
  removes any session token whose owning launcher pid is dead (signal-0 probe, same
  as the lease reaper). Run at **daemon boot** (`dispatcher::run`) and **launcher
  boot** (`codeagent::launch`, before anything else). **Proven live:** an orphaned
  token (dead owner pid) authorized its own obs before the sweep; after the launcher
  reaped it, the credential is refused at CONNECT (`bad/unknown session token` →
  `NotAuthorized`). A SIGKILL can no longer leave a usable credential.

- **(LOW, FIXED) CLI/doc consistency.** Flattened `elanus code launch <tool>` to the
  documented `elanus code <tool>` form (`src/main.rs` `Cmd::Code { tool, args }`);
  `hook` is the reserved first word for the bridge. CLI, handoff, and module docs now
  agree.

- **(LOW, DONE) Adapter factoring before Codex.** Settings generation and
  event-mapping route through the `Tool` enum (`Tool::settings`, `Tool::map_event`,
  `Tool::from_agent_noun`); the Claude generators are `claude_settings` /
  `claude_map_event`, with a `generic_event` fallback. The Codex adapter slots in by
  adding enum arms, without restructuring `launch()`/`hook()`.

- **Tests.** `cargo test` green, **93 passing** (was 86): +5 in `codesession`
  (scope/roundtrip/reap), +2 **authority** tests in `broker` (the gap the prior
  suite missed — they assert the broker ACL DENIES a session actor outside its
  scope, not just shape), with the codeagent secret-roundtrip test replaced by a
  scoped-token regression guard.

### 2026-06-20 — Codex adapter (M1 parity) via `codex exec --json` (branch `coding-agents`)

The Codex adapter reaches M1 parity with the CC adapter: the same envelope (launch,
grant-scoped per-session identity, the obs grammar, the reaper) with a **different
capture mechanism**. The `Tool` enum is the seam (`Tool::capture` →
`Capture::{HookBridge, StreamJson}`).

- **Capture = the `codex exec --json` stdout stream, NOT hooks (the key design
  decision).** Codex 0.141.0's hooks are plugin/managed-config based (Appendix B):
  `type=command` hooks need a Codex *trust review* for non-managed hooks, and the
  managed/plugin layer is a dead end for a per-launch, no-home-pollution bridge —
  there is no clean way to inject a temporary generated hook the way CC's
  `--settings` does. The clean path is `codex exec --json --skip-git-repo-check`,
  which prints a **JSONL event stream to stdout**. So the Codex adapter is
  fundamentally different from the CC adapter: where CC inherits stdio and the
  child's *hooks* call `elanus code hook` (the launcher parses nothing), the Codex
  launcher **pipes the child's stdout, reads it line-by-line as JSONL, maps each
  event, and publishes the obs record itself** — in-process, authenticating as the
  session principal (`ELANUS_PACKAGE`/`ELANUS_BUS_TOKEN` like `publish_obs` already
  sets). **No `elanus code hook` bridge for codex, no hooks.json, no `~/.codex`
  pollution at all.** Why the stream is cleaner: one process reads one stdout pipe
  in-order (no concurrent hook fan-out, no trust-review prompt, no managed-config
  file to write into the user's home), and it's strictly more observable (every
  thread item, not just the events a hook set models). Code: `run_codex_capture` +
  `codex_map_event` / `codex_map_item` in `src/codeagent.rs`; the launcher branches
  on `Tool::capture()`.

- **Launch.** `codex exec --json --skip-git-repo-check [user args…]`, cwd = the
  workdir, **keeping the user's real `CODEX_HOME`** (auth intact, zero pollution —
  setting it to a scratch would drop auth). Empty stdin (`Stdio::null` — the prompt
  comes from the user args, so the child never blocks reading stdin), piped stdout
  (parsed), inherited stderr (the human still sees Codex's own progress). The
  launcher emits its OWN `session/start` (workdir + args) before the child runs, as
  for CC.

- **Event model — confirmed live against codex 0.141.0** (one guarded
  `codex exec --json` run inside the worktree, plus the binary's serde tags).
  Top-level event types: `thread.started`, `turn.started`, `item.started`,
  `item.updated`, `item.completed`, `turn.completed`, `turn.failed`, `error`. Item
  types (`item.{type}`): **`agent_message`, `reasoning`, `command_execution`,
  `file_change`, `mcp_tool_call`, `web_search`, `todo_list`**. Mapping to the
  exec.rs obs grammar:
  - `thread.started` → `session/thread` (carries codex's `thread_id` as
    `codex_thread`) — a DISTINCT leaf, NOT a second `session/start` (the launcher
    already emitted that), so the thread id lands without a confusing double start.
  - `turn.started` → skipped (bare marker); `turn.completed` → `session/idle`
    carrying `usage` (`input_tokens`/`cached_input_tokens`/`output_tokens`/
    `reasoning_output_tokens` — the cost signal, kept); `turn.failed` / top-level
    `error` → `session/idle` with the error.
  - `command_execution`: `item.started` → `tool/command_execution/call`,
    `item.completed` → `tool/command_execution/result` (carries `command`,
    `exit_code`, `failed = exit_code != 0`, `aggregated_output`), so a shell command
    reads like CC's Bash call→result pair.
  - `file_change` (completed) → `file/write` (carries the `changes` array: path +
    add/update/delete).
  - `mcp_tool_call` → `tool/<tool_name>/{call,result}`; `web_search` →
    `tool/web_search/result`; `agent_message` → `assistant/message`; `reasoning` →
    `assistant/reasoning`; `todo_list` → `assistant/todo`.
  - `item.updated` is a streaming partial → skipped (the completed item carries the
    settled state). Any unmodeled event type or item type still lands generically
    (`event/<type>` / `item/<type>`) — like CC's `generic_event` — so nothing is
    dropped. A non-JSON stdout line (shouldn't happen under `--json`) lands as
    `event/codex_nonjson_line` rather than being dropped.

- **Sandbox stance — unchanged from the CC adapter, deliberately.** We do NOT pass
  `--dangerously-bypass-approvals-and-sandbox`: Codex keeps its OWN sandbox active
  (Seatbelt/bubblewrap, `read-only`/`workspace-write`/`danger-full-access`), exactly
  as the CC adapter keeps CC's sandbox. The complete elanus cage (write + read +
  egress) is the deferred core prerequisite for the single-cage bypass; until it
  lands, bypassing onto today's write-only fence would be a containment regression.
  elanus owns the workdir + observation + identity; the tool owns containment.

- **Identity — same scoped-token path end to end.** The codex session publishes as
  `sender = code-<session>`, scoped to its own `obs/agent/codex/<session>/#`
  (`codesession::mint` derives the scope from the agent noun `codex`). The launcher
  publishes the mapped events in-process by setting `ELANUS_PACKAGE`/
  `ELANUS_BUS_TOKEN` (the grant-scoped token) before each `buscli::publish`, so the
  broker stamps the session — never the owner.

- **Tests + verification.** `cargo test` green, **105 passing** (was 93): +12 in
  `codeagent` covering the JSONL → obs mapping (thread.started ≠ a second
  session/start; turn.completed usage; command call→result + non-zero-exit failure;
  file_change → file/write; mcp_tool_call by tool name; web_search/reasoning/
  todo_list; turn.failed + top-level error; unknown event/item lands generically;
  the capture-strategy/settings-per-tool seam). **ONE guarded live run** through the
  launcher (isolated worktree daemon on the worktree root `~/.elanus/wt-coding-agents`,
  broker `:1893` — the main `~/.elanus/root` daemon was never touched): a real
  `codex exec --json` "echo elanus-codex-probe" run produced the full ordered,
  broker-stamped record — `session/start → session/thread → tool/command_execution/
  call → tool/command_execution/result (failed:false, exit_code:0, output
  "elanus-codex-probe\n") → session/idle (turn.completed usage) → session/stop
  (exit_code:0)` — **every record stamped `sender = code-<session>`** (verified in
  the worktree root's `trace.jsonl`, the broker's verified-sender ledger). `~/.codex`
  config **untouched**: `config.toml` and `auth.json` mtimes unchanged (elanus wrote
  no config, auth intact); only codex's own `models_cache.json` refreshed on startup
  (same size, the tool's own state — exactly the CC-adapter guarantee: "elanus
  changes no user config," the tool still writes its own session/cache state).

### 2026-06-20 — M2-A: durable resumable sessions + the resume primitive (branch `coding-agents`)

The foundation for inbound delivery (M2): a session can be **resumed** after the
launcher exits, while preserving the verified "no idle live credential" property.

- **The split-session model — durable RECORD vs ephemeral TOKEN (the load-bearing
  decision).** The durable half is a new `code_sessions` row in `elanus.db`
  (`src/db.rs`): `elanus_session` (code-<id>) ↔ `native_session` (codex `thread_id`
  / CC `session_id`) ↔ `tool` ↔ `agent_noun` ↔ `workdir` ↔ created/last_active. It
  carries **NO secret** and survives process exit. The ephemeral half is unchanged
  in spirit (`src/codesession.rs`): a fresh grant-scoped, **emit-only** token minted
  at the START of each run AND each resume, retired at end, reaped on crash. So an
  idle resumable session = a record with no live token. Record read/write/touch live
  in `src/codesession.rs` (`SessionRecord`, `upsert_record`, `read_record`,
  `touch_record`); the table is created by the idempotent `init_schema`.

- **When the record is written.** Once the native session id is known, from the SAME
  observation point that already surfaces it (no new tool round-trip): **codex** on
  `thread.started` (the `thread_id`, persisted inside the stdout-capture loop —
  `capture_codex_stream`, factored out of `run_codex_capture` so launch and resume
  share it); **CC** on the `SessionStart` hook (`session_id` + `cwd` from the hook
  payload, in `codeagent::hook`). Upsert is keyed by the elanus session, so a
  re-observed native id refreshes in place rather than duplicating.

- **The resume primitive — `elanus code resume <elanus_session> "<message>"`**
  (`codeagent::resume`, wired as a reserved first word in `src/main.rs` `Cmd::Code`
  alongside `hook`). It: reaps orphans (like launch) → reads the record → mints a
  FRESH scoped emit-only token → publishes a `session/resume` marker under the SAME
  elanus session → runs the tool's native resume in the recorded **workdir** (set as
  the child cwd) capturing the result stream under the SAME `obs/agent/<agent>/<sess>/#`
  tree → retires the token → bumps `last_active`. The native resume commands
  (built by the pure, unit-tested `resume_command`):
  - **Codex:** `codex exec resume <thread_id> --json --skip-git-repo-check "<msg>"`
    — confirmed against codex-cli 0.141.0 (`codex exec resume [SESSION_ID] [PROMPT]`,
    takes an id OR `--last`; we pass the recorded id). It has **no `--cd`**, so the
    workdir is applied as the child cwd. Its `--json` stream is identical to launch
    (thread.started for the resumed thread, item.*), reusing `capture_codex_stream`
    (`record_thread=false` — the record already exists).
  - **Claude Code:** `claude -p --resume <session_id> --output-format stream-json
    --verbose "<msg>"` — confirmed against CC 2.1.183. **Decision: parse the JSONL
    print stream, do NOT rely on hooks.** A bare `-p --resume` does not reload the
    launch-time generated `--settings` hooks (that scratch is cleaned up at the
    launch's end), so CC resume captures like codex: a new `capture_claude_stream` +
    `claude_stream_map` maps the print grammar (`system/init` → `session/started`
    ONCE; `assistant`/`user` content blocks → `assistant/message` /
    `tool/<name>/call` / `tool/result`; `result` → `session/idle` with the answer +
    usage). A subtype guard drops the non-`init` `system` frames a resume replays and
    `rate_limit_event` noise, so a long history doesn't flood the bus with duplicate
    starts (caught + fixed during the live run — the first cut emitted one
    `session/started` per replayed frame).

- **No new read authority (preserves the verified property).** The resume token is
  minted with the SAME structural scope as a launch token — publish only the
  session's own obs subtree, **subscribe nothing** (`codesession` `subscribe` is
  still empty). Resume cannot read the bus. This is deliberate: M2-B (below) gives
  the DAEMON — which already has authority — the job of reading a session's mailbox
  and driving resume; the session never gains read authority. M3's interactive-pull
  read grant remains deferred.

- **M2-B (DONE 2026-06-20): daemon-driven inbound delivery.** M2-A built the resume
  primitive so it could be invoked directly (and tested); M2-B (the next Log entry
  below) wired the daemon to drive resume automatically when a message lands on a
  session's mailbox (`in/agent/<noun>/<conv>`): the daemon (with authority) reads the
  delivery and calls `resume_capture`, draining it into a resumed turn. The session
  token stays emit-only throughout — only the daemon reads.

- **Tests + verification.** `cargo test` green, **112 passing** (was 105): +3 in
  `codesession` (record round-trip + no-secret; upsert refresh keyed by elanus
  session + touch; resume mints-fresh-then-retires = no idle credential) and +4 in
  `codeagent` (codex resume command targets the recorded thread; claude resume
  command resumes the recorded session headlessly; claude print-stream → obs grammar
  incl. the non-init/rate-limit drops; resume errors cleanly with no record). **Live
  evidence (isolated worktree stack, root `~/.elanus/wt-coding-agents`, broker
  `:1893`; the main `~/.elanus/root` daemon was never touched):**
  - **Codex:** launched `code-198d81dd` → durable record written (native thread
    `019ee27c-9aed-7432-…`); `elanus code resume code-198d81dd "what phrase did I
    ask for?"` produced a NEW ordered record under the SAME elanus session, every
    record stamped `sender = code-198d81dd` (verified in `trace.jsonl`), with
    `session/thread` showing the SAME `codex_thread=019ee27c-…` (the native session
    CONTINUED, not a new one) and the model **recalling the launch turn's phrase**
    (`assistant/message "elanus-m2a-launch"`) — proof the resume targeted the right
    thread with its history.
  - **Claude Code:** launched `code-278e4576` → record written via the SessionStart
    hook (native CC session `4356b466-…`); `elanus code resume code-278e4576 …`
    produced a tidy ordered record (one `session/started`, `assistant/message`,
    `session/idle result`) under the SAME elanus session, stamped
    `sender = code-278e4576`, targeting the SAME native session, with the model
    **recalling `cc-clean-launch`** — continuity proven on both adapters.
  - **No idle credential** after either resume (the `code-sessions/` token store is
    empty — the per-resume token was retired); **`last_active` bumped** on the record
    (codex: launch `00:44:37` → resume `00:45:15`); **`~/.codex` and `~/.claude`
    config untouched** (`config.toml`/`auth.json` and `~/.claude/settings.json`
    mtimes all predate the run); the **reaper still covers crashes** (resume reaps
    orphans on entry, same as launch; the reap test passes).

### 2026-06-20 — M2-B: daemon-driven inbound delivery (mailbox → resume) (branch `coding-agents`)

A message addressed to an idle coding session's mailbox makes the daemon resume
that session with the message — closing "deliver → the session acts → result
observed." The session never gains read authority; the DAEMON (the kernel, which
already has authority) reads the delivery and drives the M2-A resume primitive.

- **Addressing — `in/agent/<tool>/<conv>`.** A session's mailbox is `in/agent/<tool>/
  <conv>` where `<tool>` is the agent NOUN (`codex` / `claude-code`) and `<conv>` is
  the elanus session `code-<id>` (the conversation locator — symmetric with the
  session's telemetry `obs/agent/<tool>/<session>/...`, same first locators). Payload
  carries the message as `{"prompt":"…"}` (a `text` alias and a bare JSON string are
  also accepted). Recognition is `codeagent::recognize_delivery(root, topic)`: exactly
  four levels `in/agent/<tool>/<conv>`, `<conv>` decodes (inverse of
  `topic::encode_segment`) to a valid `code-*` principal with a `code_sessions` record,
  AND `<tool>` equals that record's `agent_noun` (a mismatched noun is ignored, never
  cross-driven). Everything else returns None → left for the normal dispatch path.

- **Dispatcher integration — two new tick steps, between `reap` and
  `resume_suspended`** (`src/dispatcher.rs`):
  - `drive_code_deliveries` runs BEFORE `dispatch_pending`. It SQL-prefilters
    `state='pending' AND type LIKE 'in/agent/%'`, calls `recognize_delivery` on each,
    and for a match: marks the event `running` (durable claim), pulls the message
    (`delivery_message`), and enqueues a `CodeJob` on the session's worker. A
    recognized delivery with no prompt/text settles `done` (delivered, nothing to
    resume on). An UNRECOGNIZED `in/agent/*` event is left `pending` and falls through
    to `dispatch_pending`, which marks it `done` as a no-consumer event (the existing
    behavior) — so an ordinary agent mailbox or a never-recorded `code-*` conv is
    ignored cleanly (no panic, no spurious resume).
  - `settle_code_deliveries` runs after `reap`. It drains the workers' completion
    channel and settles each delivery event `running → done` (success) / `failed`
    (errored or non-zero/timeout exit — the message WAS delivered and acted on, so it
    is not re-driven), drops it from the in-flight `claimed` set, emits a small
    `obs/agent/code/delivery/complete` threaded by the delivery's `correlation_id`
    (a waiter / M4 planner reads it), and retires the now-idle worker.

- **Serialization + non-blocking — one worker THREAD per session, FIFO queue.**
  Resume runs in-process (it mints/retires the scoped token and parses the JSONL
  stream), so the fork/exec-and-reap model the rest of the dispatcher uses doesn't
  fit. Instead each session gets a dedicated `code-driver-<session>` thread that owns
  a `mpsc` FIFO: a given session runs exactly ONE resume at a time (the native tool
  isn't concurrent-safe on one thread), two rapid deliveries to the same session
  SERIALIZE behind that single thread, and a slow resume never stalls the tick loop or
  another session (the tick only enqueues + later drains a channel; it never blocks on
  a turn). The worker opens its OWN db connection inside `resume_capture` and never
  touches the dispatcher's connection. **At-least-once durability** rides the ledger:
  the claim is marked `running` BEFORE hand-off, so a daemon restart mid-resume
  re-pends it (boot's `state='running' → 'pending'` sweep) and replays it; the
  in-process `claimed` set stops a same-tick/same-process double-claim while the row is
  still visible as the worker drains it. **No double-run, no lost message.**

- **No new authority — `resume_capture` (no `process::exit`).** The M2-A CLI `resume`
  called `std::process::exit` on a non-zero tool exit — which would KILL the daemon if
  driven in-process. Refactored: `resume_capture` returns a `ResumeOutcome { success,
  exit_code }`; the CLI `resume` is a thin wrapper that still propagates the code via
  `process::exit` (script behavior unchanged), and the daemon uses `resume_capture`,
  which never exits. The driven resume mints the session's own emit-only scoped token
  exactly as before — publish only its own obs subtree, **subscribe nothing** — so the
  session gains NO read authority; only the daemon reads the mailbox. The token is
  retired after each resume and reaped on crash (the M2-A reaper covers a crash
  mid-delivery — `resume_capture` reaps orphans on entry, the daemon reaps at boot).

- **Bounded — `timeout` wraps every native call.** Both the CLI and daemon resume
  paths now wrap the native command in `timeout -s TERM <secs> codex|claude …`
  (default 600s, override `ELANUS_CODE_RESUME_TIMEOUT_S`) so a hung model turn is
  killed rather than holding a worker (or a CLI run) open forever (the handoff
  guardrail). `timeout` exiting 124 reports as a failed (timed-out) resume.

- **What's now possible:** publish `in/agent/<noun>/<code-session>` with
  `{"prompt":"…"}` → the daemon resumes the session with that message → the model acts
  → a new ordered obs record lands under the same session, stamped
  `sender = code-<session>`, plus a completion obs threaded by correlation. M4 (the
  planner side — an agent sending work + waking on the completion obs) is NOT built
  here. M3 (interactive pull / a session read grant on its own inbox) is still
  deferred — M2-B keeps the session emit-only.

- **Tests + verification.** `cargo test` green, **120 passing** (was 112): +4 in
  `codeagent` (recognition matches a recorded mailbox / rejects non-session +
  wrong-noun + malformed addresses / decodes an encoded conv segment; `delivery_message`
  accepts prompt/text/bare-string and rejects empty), +4 in `dispatcher` (drive claims
  a recognized delivery + leaves an unrecognized one for dispatch; the same delivery is
  never enqueued twice; one worker thread serializes its FIFO with no overlap; settle
  marks done/failed + retires the idle worker). **Live evidence (isolated worktree
  stack, root `~/.elanus/wt-coding-agents`, broker `:1893`; the main `~/.elanus/root`
  daemon on `:1883` was never touched — verified zero `m2b-*` events + no session row
  in the main root):**
  - Launched codex `code-41e2e011` (workdir `/tmp/ca-m2b-work`, native thread
    `019ee293-…`) with codeword `ELANUS-M2B-LAUNCH-7`, then went idle (no live token).
  - `bus pub in/agent/codex/code-41e2e011 {"prompt":"What was the secret codeword…?"}`
    (as owner, `--correlation m2b-deliver-1`) → the daemon recognized it (event 23,
    `delivery/accepted`), resumed the SAME native thread `019ee293-…`, and the model
    **recalled `ELANUS-M2B-LAUNCH-7`** (`assistant/message`) — proof the resume targeted
    the right thread with its history. A NEW ordered record (`session/resume →
    session/thread → assistant/message → session/idle`) landed under the same elanus
    session, every record stamped **`sender = code-41e2e011`**; `last_active` bumped
    (`01:09:18 → 01:09:55`); `delivery/complete` emitted (`cause_id=23`,
    `correlation=m2b-deliver-1`, `failed:false`).
  - **Serialization:** two rapid deliveries (`m2b-serial-A`, `m2b-serial-B`) were both
    accepted in the same tick (`01:10:39.529` / `.531`) but completed SEQUENTIALLY
    (`A` `01:10:45.736`, then `B` `01:10:52.187` — ~6.5s apart, no overlap), the model
    replying `SERIAL-A-DONE` then `SERIAL-B-DONE` in order — the single worker thread
    serialized them, no corruption.
  - **Non-existent / non-session deliveries ignored cleanly:** `in/agent/codex/
    code-deadbeef` (never recorded) and `in/agent/kestrel/c999` (ordinary agent) both
    settled `done` with ZERO resume attempts and no panic — the daemon stayed up.
  - **No idle credential** after any resume (`.secrets/code-sessions/` empty — the
    per-resume token retired); **`~/.codex` config untouched** (`config.toml` /
    `auth.json` mtimes predate the run).

### Still TODO (next increments)
- **M3 interactive-pull / session read grant.** When a session is given read
  authority over its own inbox, extend its `subscribe` scope in `codesession`
  (today empty — emit-only). M2-A deliberately did NOT do this.

- Posture-mode → cage mapping (Codex read-only/workspace-write/full ↔ cage write +
  read + egress) and the complete cage (incl. read scoping) before bypass: BLOCKED
  on the docs/sandbox.md end-state cage; deferred per the stance above. Applies to
  both adapters (each keeps its own sandbox until the end-state cage lands).
- M2 inbox (inbound delivery) for both adapters — the next milestone. Interactive
  pull vs headless resume (`codex exec resume`, `claude -p --resume`). When M2 grants
  a session *read* authority (its inbox), extend its `subscribe` scope in
  `codesession` (today it is empty — emit-only).
- Codex context-injection placement (M3): confirm whether Codex's prompt-hook
  injection lands as developer/system vs user text, and prefer the out-of-band
  channel for parity with CC's system-reminder seam. (Codex's plugin hooks were a
  dead end for *capture*; M3 injection is a separate question to re-verify.)
- ~~Codex adapter (M1 parity)~~ — **DONE 2026-06-20** via `codex exec --json` (see
  the Log entry above). Hooks were ruled out (plugin/managed dead end); the stdout
  JSONL stream is the capture path.
- Context-injection placement per tool + measured cache behavior (M3, the
  `UserPromptSubmit` `additionalContext` system-reminder seam).
- Inbound-delivery mechanism (M2 inbox: interactive pull vs headless `--resume`).
  Note: when M2 grants a session *read* authority (its inbox), extend its
  `subscribe` scope in `codesession` (today it is empty — emit-only).
- ~~Grant-scoped per-session actor token~~ — **DONE** in the fix pass (structural
  scope on the broker, no daemon RPC; see the fix-pass Log above). M0's complete-cage
  criteria (read/egress denial) remain deferred per the sandbox stance above — the
  authority gap is closed, the OS-cage bypass is still gated on the end-state cage.
