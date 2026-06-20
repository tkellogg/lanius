# Handoff: coding-agent support (Codex & Claude Code)

Status: **M0+M1 landed for BOTH adapters; M2-A (durable resumable sessions + the
resume primitive) landed 2026-06-20; M2-B (daemon-driven inbound delivery off a
session mailbox → resume) landed 2026-06-20; M4-A (the orchestration loop closing:
requester capture → completion routing → idempotency) landed 2026-06-20** — Claude
Code (hook bridge) 2026-06-19, Codex (`codex exec --json` stdout stream) 2026-06-20,
branch `coding-agents`. All verified end to end against the live worktree stack.
M4-B (the mediated dispatch tool + the launch-envelope briefing) landed
2026-06-20; **M3 (per-turn context injection + the session's own-inbox read)
landed 2026-06-20** — the first increment giving a session any read capability,
scoped own-inbox-only by an env-derived ledger query (the bus token stays
emit-only). **M5 (advisory peer coordination — rooms + edit claims surfaced via the
M3 injection, crash-released) landed 2026-06-20 — this completes M0–M5.**
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

### Two operating modes, and the planner symmetry

A coding session runs in one of two modes, and the difference is entirely **who
drives its turns**. It is not two kinds of session — it is one durable session
advanced two ways.

- **Interactive.** A human sits at the real TUI and drives the turns. elanus owns
  the cage, *observes* the session (hooks/stream → bus), and *injects context* into
  each turn (the system-reminder seam, M3) — but it does not and cannot push a turn
  in. There is no supported way to inject a turn into a running interactive TUI
  (confirmed for both tools). So anything elanus wants the session to notice — a
  message in its mailbox, a coordination claim — can only surface as injected
  context on the *next* turn, and the next turn happens only when the human submits
  one. **The human is the pump.** Right for a person working hands-on; wrong for an
  autonomous loop, which cannot advance without someone pressing Enter.
- **Headless.** elanus drives the turns. A message delivered to the session's
  mailbox makes the daemon resume the session non-interactively (`claude -p
  --resume` / `codex exec resume`) with that message; the session takes a turn and
  the turn ends. No human is in the loop; the human watches or joins through the
  elanus UI and the recorded bus stream. This is the mode the orchestration vision
  needs. **(M2-A + M2-B build exactly this for the worker side, verified.)**

**The same durable session bridges both** (M2-A). You can start interactive, let it
run headless, and resume it interactively later — same session id. So "ending
interactive mode" is not a teardown: the human just stops driving and elanus drives
subsequent turns by resume. "Never starting interactive" means launching headless
from the first turn for any session meant to run in a loop. A session destined for
orchestration should go headless from the start; a session a human is shepherding
can hand off to headless when they step away.

**The planner is the same kind of thing as the worker.** When a coding agent acts
as a *planner* — hands work to a worker and reacts to the result — the clean
realization in headless mode is that the planner is *itself* a resumable session: it
takes a turn, dispatches work to a worker's mailbox, and **ends its turn**; when the
worker completes, that completion is delivered to the *planner's* mailbox, which
resumes the planner for the next step. Planner and worker ride the identical
"message arrives → daemon resumes the session" machinery (M2-B); the only difference
is who is waiting on whom, and a session can be a worker in one relationship and a
planner in another. (A native elanus agent can also be the planner — it waits by
suspending/resuming on elanus's own machinery instead of by ending a headless turn —
but a coding agent planning another, Claude Code → Codex, is the headline case and
fits the resume model directly. See M4.)

**elanus briefs the session on the envelope at launch.** A coding agent does not, on
its own, know it is running under elanus, that it may be resumed headlessly, or how
hand-off works — so the launcher must *tell it*. Inject an operating-envelope
briefing at launch (Claude Code: `--append-system-prompt`; Codex: the equivalent
system/developer instruction) covering: you are under elanus supervision; when you
hand work off, **end your turn cleanly rather than waiting in a busy loop**; results
reach you as a resumed turn; here is how to address a worker and read your inbox;
and how to behave toward your human (who may or may not be watching). This briefing
is what makes a session a well-behaved loop participant instead of one that stalls
waiting for a turn that will never come on its own. The one-time briefing rides the
launch flag; the per-turn ongoing context (inbox status, claims) rides the M3
injection seam.

The honest caveat for any driven loop: delivery is **at-least-once** (M2-B), so a
daemon crash mid-resume can replay a turn. A planner must read the actual recorded
state before acting, not assume each wake-up is unique. Idempotency hardening (a
delivery key) is part of M4.

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

This is the **per-turn** counterpart to the one-time launch-envelope briefing
(operating-modes section): the briefing tells a session how the envelope works once,
at launch; M3 keeps it informed every turn. It is also what makes interactive mode's
inbox-*pull* work (the human-pumped "you have N messages" surfacing) and what M5
points at to surface coordination claims.

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

### M4 — The orchestration loop: planner drives worker

Builds directly on M2-B (deliver → resume) and the operating-modes/planner-symmetry
section above. The planner is itself a resumable session; closing the loop is mostly
wiring + a briefing + a mediated dispatch tool.

Shape:
- **Close the loop M2-B leaves open:** route a worker's completion back to **the
  deliverer's mailbox** (carrying the delivery's `correlation_id`), so a planner
  session is resumed to react. Today M2-B emits `obs/agent/code/delivery/complete`
  but does not deliver it to whoever asked — that reply is the missing wire. The
  completion delivered to the planner's mailbox is itself an M2-B delivery, so it
  resumes the planner exactly like any other.
- **Give the planner a mediated way to dispatch work:** a tool the planner calls —
  an elanus MCP tool or `elanus code deliver <session> "<msg>"` — that performs the
  delivery to the worker's mailbox **with elanus's authority** and records the
  planner as the requester. Do NOT widen the session's emit-only token to publish
  into other mailboxes; route authority through the mediated tool (the planner asks
  elanus to deliver, elanus does it and records who asked).
- **Launch-envelope briefing** (operating-modes section): so the planner ends its
  turn after dispatching and is resumed on completion, rather than busy-waiting.
- **Idempotency:** stamp each delivery with a key so a replayed completion/turn (the
  at-least-once duplicate on a mid-resume crash) is recognized and not double-acted;
  the planner reads recorded state before acting.
- The planner may be a coding agent (Claude Code → Codex, the headline) or a native
  elanus agent (which waits by suspend/resume). Build for the coding-agent case;
  note the native-agent path.

Acceptance criteria:
- A planner (a coding session) runs ≥2 steps of a real task **fully headless**: it
  dispatches to a worker, ends its turn, is resumed by the worker's completion,
  reviews, and dispatches the next — with no human acting as the wire. The whole
  loop is observable on the bus; the human can watch in the UI and join/interrupt,
  and resume any session later (M2-A).
- A worker failure reaches the planner (a failed completion) rather than hanging the
  loop.
- A replayed delivery (simulate a mid-resume crash) does not double-act: the
  idempotency key makes the second run a recognized no-op.
- The planner dispatches through the mediated tool (elanus authority, planner
  recorded as requester); the session token stays emit-only.

### M5 — Peer coordination over the bus (advisory)

**Depends on M3.** Surfacing claims to a session is the M3 injection seam pointed at
a new computed block, so M3 lands before M5. Build order from here: **M4** (close the
loop) → **M3** (the per-turn injection seam: inbox status + claims) → **M5** (the
coordination room that M3 surfaces). The M4 launch briefing can use the launch flag
without M3; the per-turn surfacing cannot.

Shape:
- Multiple concurrent coding sessions share a coordination room (`in/group/<id>`,
  ledger-backed). A session announces an edit claim ("editing src/foo.rs"); each
  session's M3 injection shows the current claims in its prompt. Advisory only — no
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

### 2026-06-20 — M4-A: the orchestration loop closes (requester capture → completion routing → idempotency) (branch `coding-agents`)

M2-B delivered work to a worker and observed the result but left the loop open: a
worker's completion was never routed back to whoever asked, so a planner could not
be woken to react. M4-A closes that loop — **a worker's completion is delivered to
the requester's mailbox, which (when the requester is itself a coding session)
resumes the planner via the SAME M2-B machinery** — plus a durable idempotency key
so the at-least-once duplicate can't double-act. The planner symmetry holds: a
planner is just a resumable session whose mailbox carries the worker's completion.
**M4-B (the mediated dispatch tool + the launch-envelope briefing) is deferred** —
see the note at the end of this entry. (Built + verified in the isolated worktree
stack: root `~/.elanus/wt-coding-agents`, broker `:1893`; the main `~/.elanus/root`
on `:1883` was never touched — its `elanus.db` has no `code_sessions` table at all,
so it could not have been.)

- **Requester capture — where.** `drive_code_deliveries` (src/dispatcher.rs) now
  reads the materialized delivery row's broker-verified `sender` and JSON `payload`
  alongside the topic. `codeagent::delivery_requester(root, payload, sender, corr)`
  resolves who to reply to, with precedence: (1) an explicit `reply_to` in the
  payload — a full `in/` topic verbatim (must be wildcard-free), or a bare actor
  name expanded to that actor's mailbox; else (2) the broker-verified `sender`.
  A coding-session requester (`code-*` with a durable record) resolves to its OWN
  session mailbox `in/agent/<its-noun>/<session>`, so routing the completion there
  resumes it. The `kernel`/`owner` senders are NOT planners waiting on a completion
  → None (a plain owner delivery routes nothing — the M2-B behavior, unchanged).
  The captured requester rides the in-flight `CodeJob` → `CodeDone` so the settle
  step can route it. **The session token is NOT widened** — the daemon (which
  already holds authority) does the routing emit; the session stays emit-only.

- **Completion routing — where.** `settle_code_deliveries` (src/dispatcher.rs),
  after the existing `obs/agent/code/delivery/complete` obs (kept), now — if the
  delivery named a requester — emits a delivery to that requester's mailbox carrying
  the SAME `correlation_id`, a `prompt` (what resumes a coding-session planner), the
  success/failure flag, a concise `result` line, and a `worker_obs` pointer to the
  worker's `obs/agent/<noun>/<session>/#` subtree (so the planner reads recorded
  state before acting — the honest-state guidance). It is a kernel-minted ledger
  event (`sender=kernel`), inserted `pending`, so `drive_code_deliveries` picks it up
  the next tick and resumes the planner exactly like any other delivery — that is
  the loop closing, reusing M2-B end to end with no new path. **A worker FAILURE
  routes too** (the synthetic-failure paths in `enqueue_code_job` carry the
  requester), so a planner's loop is not left hanging on a worker that never started.

- **Idempotency — how it dedupes + survives restart.** Each delivery carries a key:
  `codeagent::idempotency_key(payload, event_id)` = an explicit payload
  `idempotency_key` if present, else `event:<inbound-event-id>` (stable across the
  at-least-once replay, which re-pends the SAME row with the SAME id). A new DURABLE
  table `code_delivery_keys` (src/db.rs, keyed by the key) records a processed
  delivery; `codesession::claim_delivery_key` does an atomic
  `INSERT … ON CONFLICT DO NOTHING` (returns true only for the first claimant), and
  `delivery_key_seen` is the read-side check. In `drive_code_deliveries` a delivery
  whose key is already present is settled `done` as a recognized no-op
  (`obs/agent/code/delivery/duplicate`, `reason: already processed`) — NO second
  resume. The row is durable, so the replay a daemon-crash-mid-resume re-pends
  (boot's `running → pending` sweep) is caught across the restart, not just a
  same-process duplicate. The routed completion also carries
  `idempotency_key = code-complete:<worker-event-id>` so a replayed completion
  dedupes when it drives the planner.

- **Live evidence — the deliver→worker→completion→planner-resume chain.** Two real
  idle codex sessions: worker `W = code-e4da979b` (`/private/tmp/m4a-worker`),
  planner `P = code-021d43f4` (`/private/tmp/m4a-planner`). Published to `W`'s
  mailbox `{"prompt":"…M4A-WORKER-DID-THE-WORK…","reply_to":"in/agent/codex/
  code-021d43f4"}` (`--correlation m4a-loop-1`, as `owner`). Result, all from the
  worktree-root ledger/`trace.jsonl`:
  - **W resumed and acted** — `assistant/message "M4A-WORKER-DID-THE-WORK"`, stamped
    `sender=code-e4da979b`. The delivery (event 75) settled `done`.
  - **The completion routed to P** — event 78, `in/agent/codex/code-021d43f4`,
    `sender=kernel`, **same `correlation_id=m4a-loop-1`**, payload `{prompt, failed:
    false, worker:"code-e4da979b", worker_obs:"obs/agent/codex/code-e4da979b/#",
    idempotency_key:"code-complete:75"}`.
  - **P was then resumed and reacted** — a NEW turn, `sender=code-021d43f4`,
    "I'll read the worker transcript/state first… Given the completion report says
    success…". That chain (deliver → worker resume → completion → planner resume),
    each session stamped with its own `sender=code-<session>`, is the headless loop.

- **Live evidence — idempotency (the no-op).** Re-published the same delivery twice
  with an explicit `idempotency_key:"m4a-dup-key-1"` (`m4a-dup-A`, `m4a-dup-B`): the
  first (event 80) was `accepted` and drove ONE resume (W: "Understood."); the second
  (event 81, same key) was the recognized `duplicate` (`reason: already processed`),
  settled `done`, **NO second resume** (W's assistant-turn count went 2→3, not →4).

- **Live evidence — the mid-resume crash replay.** Published a delivery
  (`idempotency_key:"m4a-crash-key"`, event 83); SIGKILL'd the daemon; forced the
  event back to `running` (the state a crash leaves — claimed, never settled, key
  recorded); restarted. The boot sweep re-pended event 83 (`running → pending`); the
  durable key made the re-drive a recognized no-op (`duplicate`, `already
  processed`), event 83 settled `done`, **W's turn count unchanged at 3, zero resume
  of W in the restarted daemon log**. The replay was caught across the restart.

- **No regression.** A normal delivery to `W` with no requester (owner, no reply_to)
  resumed the worker (W 3→4 turns) and routed NOTHING (P unchanged, zero routed
  events). Ordinary agent dispatch (`in/agent/kestrel/conv-xyz`) settled `done` via
  the existing no-consumer `dispatch_pending` path, never touched the coding path,
  daemon stayed up (no panic). **No idle credential** after (`.secrets/code-sessions/`
  empty); **`~/.codex` config untouched** (`config.toml` 2026-06-19 09:33,
  `auth.json` 2026-06-16 22:00 — both predate the run); the main root never touched.

- **Tests.** `cargo test` green, **129 passing** (was 120): +5 in `codeagent`
  (`idempotency_key` precedence; `delivery_requester` from explicit reply_to topic /
  from a coding-session sender → its own mailbox / None for owner-kernel-unrecorded /
  native-agent sender uses the correlation conv), +1 in `codesession`
  (`claim_delivery_key` is once-and-durable, the replay loses, survives a fresh
  connection), +3 in `dispatcher` (drive captures the requester from the sender; a
  replayed delivery with the same key is a no-op with no second job; settle routes a
  completion to the requester's mailbox, pending, same correlation, with the prompt +
  idempotency key).

- **Deferred to M4-B (NOT built — noted per the spec):**
  - **The mediated dispatch tool** — a CLI/MCP a planner calls (e.g. `elanus code
    deliver <session> "<msg>"` or an elanus MCP tool) that performs the delivery to a
    worker's mailbox **with elanus's authority** and records the planner as the
    requester, so the session's emit-only token never has to publish into another
    mailbox. M4-A only wired the loop that closes once a delivery is in a worker's
    mailbox; how a coding-session planner *originates* that delivery (without
    widening its token) is M4-B. (Today a planner's delivery would have to be placed
    by the human/owner or a native agent; the routing back is what M4-A added.)
  - **The launch-envelope briefing** — injecting the operating-modes briefing at
    launch (`--append-system-prompt` for CC; the Codex equivalent) so a planner ends
    its turn cleanly after dispatching and is resumed on completion rather than
    busy-waiting. The one-time briefing rides the launch flag (it does not need M3).
  - Honest residual: the settle UPDATE and the routed emit are not one transaction,
    so a daemon crash in that tiny gap settles the worker delivery but loses the
    route (the planner would not be woken) — at-most-once for the route itself, the
    same property the `obs/.../complete` emit already had. Acceptable for M4-A
    (the spec's idempotency goal is "don't double-act"); a transactional route or a
    re-derivable-from-`done` route is a hardening for later if a lost wake bites.
    **CLOSED 2026-06-20 by the boot reconciliation `reconcile_lost_routes` — see the
    M4-A fix-pass Log entry below.**

### 2026-06-20 — M4-A fix pass: confused-deputy `reply_to`, cross-victim suppression, lost-wake recovery (branch `coding-agents`)

The M4-A adversarial verify turned up two MEDIUM security bugs and confirmed the
disclosed lost-wake residual was worth closing. All three fixed; `cargo test` green
(134, was 129); each proven live in the isolated worktree stack (root
`~/.elanus/wt-coding-agents`, broker `:1893`) with **no model turn** (fail-fast
resume on a bogus workdir — the failure-routes path). The session token stays
emit-only; the daemon still routes with its own authority, now to a constrained,
validated destination (no authority widened). Security ledger: docs/security.md
entry 21.

- **(MED, FIXED — security) Confused-deputy `reply_to`.** `delivery_requester`
  accepted an explicit payload `reply_to` that merely `starts_with("in/")` and was
  wildcard-free and routed a **kernel-authored** completion to it **verbatim** — so
  `reply_to: in/human/owner` (or any `in/...`/`signal/`/`obs/`/`work/` topic) made the
  daemon publish a kernel message to the human inbox or an arbitrary topic. Fix
  (src/codeagent.rs): an explicit `reply_to` now must **resolve to a recognized
  actor's mailbox** the same safe way the sender path does — new `resolve_reply_to`
  routes through `mailbox_for_actor`, accepting a bare actor NAME or a full
  `in/agent/<noun>/<conv>` mailbox (the actor is extracted and the mailbox
  **re-derived**, never used verbatim), and rejecting raw/arbitrary `in/...`,
  `in/human/*`, `in/group/*`, `signal/`, `obs/`, wildcards, path-unsafe names, and
  unrecorded `code-*` convs (→ None, no route). `mailbox_for_actor` now requires
  `valid_principal`. Proven live: `reply_to: in/human/owner` and `in/totally/
  arbitrary/x` both captured `reply_to:null` (refused) — the `in/human/owner` kernel
  count unchanged, the arbitrary topic empty; a legit `reply_to: code-<planner>`
  still routed to that planner's own mailbox.

- **(MED, FIXED — security) Cross-victim idempotency suppression.**
  `code_delivery_keys` was keyed on `idempotency_key` alone (global), so an attacker
  pre-claiming an explicit key `K` (for their own session A) silently suppressed a
  *different* victim's delivery to session B that reused `K` (B settled `done` as a
  bogus duplicate, never driven). Fix (src/db.rs + src/codesession.rs): namespace by
  target session — `PRIMARY KEY (session, idempotency_key)`, claim/lookup per session.
  An explicit key only dedupes a delivery to the SAME session; the default `event:<id>`
  is globally unique regardless; the same-session replay dedupe still holds across a
  restart. Pre-release: the table was dropped+recreated (no migration). Proven live:
  with `K` pre-claimed for A, a victim delivery to B reusing `K` was DRIVEN
  (`delivery/accepted`, not `duplicate`), recorded independently per session; the
  genuine replay (same key+session, re-pended `running` across a SIGKILL+restart) was
  still a `delivery/duplicate` no-op with zero second resume.

- **(reliability, FIXED — the disclosed residual) Lost planner wake on a crash.** The
  settle UPDATE (worker delivery → `done`) and the routed completion emit are separate
  autocommit transactions; a crash between them settles the worker but loses the route,
  and the boot sweep only re-pends `running` events — never `done` — so the wake was
  lost forever. Fix (src/dispatcher.rs): a **boot reconciliation**
  `reconcile_lost_routes` (the cleaner fit for this crash-only codebase — it mirrors
  the existing boot sweeps for orphaned dispatches / stale leases / orphaned
  credentials, and needs no cross-connection transaction across `events::emit` +
  `read_record`). It walks the durable `code_delivery_keys` rows (each marks a delivery
  that was actually DRIVEN), re-derives the requester from the original delivery
  event's persisted `sender`/`payload`/`correlation` via `delivery_requester`, and —
  if a requester resolves and no completion was ever routed
  (`route_already_emitted`, the `(cause_id, type)` guard) — re-emits the route. The
  routing is refactored into a shared `route_completion` helper (settle + reconcile),
  idempotent via that guard + the stable `code-complete:<worker-event-id>` key. A
  recovered route carries an honest "completed (route recovered after a restart)"
  result and the worker-obs pointer — its job is to WAKE the planner to read recorded
  state (the worker transcript is durable on the bus), which it does regardless of the
  exact result text. Why reconciliation over one transaction: a single settle+route
  transaction would still lose the wake — a crash rolls the delivery back to `running`,
  whose re-drive is *deduped* by the durable key and never re-routes; reconciliation is
  what actually recovers it. Proven live: a crafted settle→route-gap crash state
  (worker delivery `done`, key recorded, no route) → restart → boot logged
  `recovered 1 lost completion route(s)`, the route landed on the planner's mailbox
  (sender=kernel, same correlation); a second restart routed nothing (idempotent).

- **Tests.** `cargo test` green, **134 passing** (was 129): +1 codeagent
  (`explicit_reply_to_cannot_target_human_inbox_or_arbitrary_topic`; the existing
  `requester_from_explicit_reply_to_topic` was updated to assert re-derivation, not
  verbatim), +1 codesession
  (`delivery_key_is_namespaced_by_session_no_cross_victim_suppression`; the existing
  key-claim test updated to the per-session signature), +3 dispatcher
  (`cross_victim_key_does_not_suppress_a_different_session_delivery`,
  `reconcile_recovers_a_route_lost_in_the_settle_route_gap`,
  `reconcile_skips_deliveries_with_no_requester`).

### M4-B — the deliver tool + the launch-envelope briefing (2026-06-20)

The loop is now **planner-originated**: a planner coding session dispatches work to a
worker with one tool call, no human as the wire. There is no authorization model here —
planner and worker are the user's own agents with homogeneous authority; the safety is
the recorded provenance, not a gate (Tim: "if you're trying to make this into a trust
issue, there isn't one"). The tool is plumbing + record, not a new bus authority.

- **`elanus code deliver <worker-session> "<message>"`** (`codeagent::deliver` +
  `record_delivery`, reserved word under `Cmd::Code`). Run from inside a coding session
  (reads `ELANUS_CODE_*`): records the running session as the **requester** so M4-A
  routes the worker's completion back to it. Writes a `pending` delivery event to the
  worker's mailbox `in/agent/<worker-noun>/<worker-session>` with the message, the
  requester as `reply_to` (a valid coding-session mailbox — passes M4-A's constraint),
  and a `code-deliver-<uuid>` correlation threading the whole round trip. The dispatch
  goes through the kernel ledger emit, so the planner's **emit-only token is not
  widened**; the daemon's `drive_code_deliveries` picks it up next tick. Refuses an
  unrecorded/unknown worker and **self-delivery** (which would self-resume into a loop),
  and an empty message — all with clear errors.
- **Launch-envelope briefing** (`briefing`, `take_brief_flag`, `codex_briefing_block`):
  injected at launch (default on; `--no-brief` suppresses). CC via
  `--append-system-prompt`; Codex as an out-of-band stdin `[elanus operating envelope]`
  block. Tells the agent: you run under elanus supervision; hand work off with
  `elanus code deliver`; **end your turn after dispatching — do not busy-wait** — the
  result returns as a resumed turn.
- Also fixed the dangling "(below)" cross-reference in `docs/security.md` entry 21.
- **Tests.** `cargo test` **141 passing** (was 134): +7 codeagent (deliver builds the
  worker delivery recording the requester; unknown-worker / self-delivery / empty-message
  clean failures; the unrecorded-requester-still-records-sender path; the briefing covers
  the envelope essentials; the `--no-brief` flag; the codex briefing block).
- **STATUS: implementation complete + unit-tested; live planner-originated loop NOT yet
  verified** (the impl agent died on an auth error before the e2e step). The adversarial
  verify does the live deliver→worker→completion→planner-resume run.

### M3 — per-turn context injection + the session's own inbox read (2026-06-20)

The per-turn counterpart to the one-time launch briefing: every turn a session is
told its inbox status and any memory note, injected OUT OF BAND (a system note,
after the cached prefix — not the user message). This is the FIRST increment where
a session gains any READ capability, and it is widened by EXACTLY one thing: a
session may read its OWN inbox. Verified live in the isolated worktree stack (root
`~/.elanus/wt-coding-agents`, broker `:1893`; the main `~/.elanus/root` on `:1883`
was never touched).

- **The read-scope approach (the crux) — a scoped LEDGER QUERY by env-derived
  identity, NOT a bus-token widening.** `codesession::inbox_for_session` selects the
  `events` rows whose topic is the session's OWN mailbox `in/agent/<noun>/<session>`,
  where `<noun>`/`<session>` come from the running session's env
  (`ELANUS_CODE_AGENT`/`ELANUS_CODE_SESSION` the launcher set) — **never from an
  argument**. So a session can never name another session's inbox: the mailbox topic
  is built from its OWN identity, exactly as `elanus code hook` publishes as itself.
  The emit-only bus token is **structurally unchanged** — `SessionToken.subscribe`
  stays `Vec::new()` (the broker ACL still denies the token any subscribe, including
  to its own inbox `#` and `obs/#`). The new read authority is the kernel-side query
  gated by the env-derived identity, the approach the M3 spec prefers (no bus-token
  widening needed). **Proven own-inbox-only, live and adversarially:** session A and
  session B each see ONLY their own deliveries; passing B's id as an arg to A's
  `inbox` is silently ignored (no session-id arg exists — flags only) and A still
  reads only A's; no env → cleanly refused; any child A spawns inherits A's env (the
  only way to "be B" is to hold B's env, i.e. to BE B). The broker session-scope ACL
  tests (`session_actor_is_scoped_by_the_broker_acl`,
  `unminted_session_actor_authorizes_nothing`) still pass — the token cannot
  subscribe to anything.

- **`elanus code inbox`** (`codeagent::inbox_cmd`, reserved word under `Cmd::Code`).
  Run from inside a session: lists THIS session's pending/unseen deliveries (message
  + who-from + correlation + state), scoped to its own env-derived mailbox by
  construction. "Seen" is tracked in a new durable `code_inbox_seen` table keyed by
  `(session, event_id)`: pulling marks the listed deliveries seen (idempotent — a
  second pull doesn't re-surface them, `INSERT … ON CONFLICT DO NOTHING`); the
  per-turn injection counts only UNSEEN. `--all` shows the full inbox
  (non-destructive, marks nothing); `--json` for a tool to parse. A session can only
  mark ITS OWN deliveries seen (the keyspace is namespaced by session).

- **Per-turn injection (`codeagent::turn_injection`)** — an out-of-band `[elanus]`
  block reporting inbox status ("N new message(s)" + a brief preview of the latest)
  and an optional memory note. None when there's nothing to say (a quiet turn injects
  nothing). Kept per-turn, OUT of the cached prefix, so it never busts prompt caching.
  Per adapter:
  - **Claude Code (interactive):** the `UserPromptSubmit` (and `SessionStart`) hook
    now prints `{"hookSpecificOutput":{"hookEventName":…,"additionalContext":…}}` on
    stdout — the **system-reminder layer** (Appendix A), NOT the user message.
    Verified live: the hook emits the inbox+note `additionalContext`, and it CHANGES
    when the inbox changes (2 unseen → pull → "no new messages" → deliver again →
    "1 new message") and when the note is edited (`src/parse.rs` → `src/parser/mod.rs`).
  - **Codex + driven CC resume (headless):** a driven resume does NOT fire the
    launch-time hooks (a bare `-p --resume`/`codex exec resume` doesn't reload the
    generated `--settings`), so the per-turn context rides the **resume prompt the
    daemon builds** — `build_resume_message` prepends the `[elanus]` block ahead of
    the delivered message, out of band. Verified live: with a note
    `ELANUS-NOTE-XK42`, a daemon-driven codex resume produced the model replying
    `ELANUS-NOTE-XK42` verbatim — proof the note rode the resume prompt and the model
    read it.

- **The per-session memory note (`code note <session> "<text>"`,
  `codesession::{set_note,get_note}`)** — a minimal stored, editable block keyed by
  session in a new durable `code_notes` table (one row per session; latest wins;
  empty text clears it). A planner (or human) leaves a worker a persistent reminder;
  it surfaces in the per-turn injection. Round-trips live (set → appears in the next
  turn's injection; change → the change shows). Refuses a non-recorded session
  (a note would otherwise sit unread).

- **Deferred from the block substrate (noted per the spec).** The full
  `context_blocks` substrate (`Placement`, registers, computed blocks, the build
  log, docs/context.md) is NOT integrated — the note is a thin, purpose-built stored
  value, and the inbox status is computed inline in `turn_injection`. A clean reuse
  of `context_blocks` (the note as a `Session`-scoped block, the inbox status as a
  computed block) is the natural next step but was not needed for M3's surface; M5's
  edit-claims block points at the same seam. Recorded as a deliberate deferral.

- **No regression.** The emit-only publish scope is unchanged (only own-inbox READ
  added, via the ledger query, not the token); launch/resume/deliver/M4-A routing
  unbroken (M4-B `code deliver` still records the requester and the daemon drives it);
  `~/.codex`/`~/.claude` config untouched (M3 writes no tool config — `config.toml`
  Jun 19, `auth.json` Jun 16 both predate the run); no idle credential after a resume.
  **Operational note (per the spec):** the launch briefing tells the agent to run
  `elanus code deliver`/`elanus code inbox`, which need `elanus` on PATH — a
  dev-from-worktree planner uses `target/debug/elanus` (the dev stack prepends
  `target/debug` to PATH; a hand-driven test must do the same or use the absolute
  path).

- **Tests.** `cargo test` green, **148 passing** (was 141): +3 codesession
  (`inbox_reads_only_the_sessions_own_mailbox` — the crux: A sees only A's, B only
  B's, a mismatched noun reads its own empty mailbox not another's, a non-session
  name has no inbox; `inbox_seen_is_idempotent_and_scopes_unseen`;
  `note_round_trips_and_clears`), +4 codeagent
  (`turn_injection_reflects_inbox_and_note_and_changes_with_state`,
  `turn_injection_shows_only_unseen_inbox`,
  `build_resume_message_prepends_injection_only_when_present`,
  `note_cmd_requires_a_recorded_session`).

### M5 — advisory peer coordination: rooms + edit claims (2026-06-20)

Multiple concurrent coding sessions share a coordination **room**; each announces
advisory edit **claims** ("I'm editing src/foo.rs"); each session's M3 per-turn
injection surfaces its ROOMMATES' current claims (excluding its own), so
cooperating workers route around each other. **This completes M0–M5.** There is NO
trust model — sessions are the user's own cooperating agents with homogeneous
authority; a claim is advisory metadata its peers read, never a lock, never a gate
(Tim's safety = honest record + work preservation, not restriction). Verified live
in the isolated worktree stack (root `~/.elanus/wt-coding-agents`, broker `:1893`;
the main `~/.elanus/root` on `:1883` was never touched).

- **Room + membership (minimal, ledger-state like M3).** A session joins a room at
  launch with `--room <id>` (the flag is parsed out and stripped before the args
  reach the tool — `take_room_flag`). The room is stored on the durable
  `code_sessions` record (a new nullable `room` column — `set_room`, preserved
  across the later native-id upsert by a COALESCE so a CC SessionStart / codex
  thread.started that carries `room:None` doesn't clear it). Membership is a row in
  a new `code_room_members` table `(room, session, agent_noun, owner_pid)` —
  `join_room`. This is the scope a session shares claims with: it SEES its
  roommates' claims and writes only its own.

- **Edit claims (advisory, never a lock).** `elanus code claim <path>` /
  `elanus code unclaim <path>` (and a read-only `elanus code claims [--json]` view),
  run INSIDE a session, env-derived identity exactly like `code inbox`/`code note`
  (`session_room_identity` reads `ELANUS_CODE_SESSION`/`ELANUS_CODE_AGENT` from the
  env the launcher set, and the room from the session's OWN record — never a
  caller-supplied argument). A claim is a row in a new `code_claims` table
  `(room, session, path)` keyed so re-claiming a path is idempotent; the raw path is
  stored verbatim in a column (a path is a noun). **Recording a claim never blocks
  anyone** — `add_claim` is a pure insert. A session can record/clear only its OWN
  claims (the room/session are its own identity); `remove_claim` is scoped to
  `(room, session, path)` so a session can never clear a peer's claim.

- **Surfaced via the M3 injection seam (the same out-of-band channel).**
  `turn_injection` now also reads the session's room (from its OWN record) and
  appends an `[elanus peers]` block listing the OTHER sessions' claims, EXCLUDING
  its own (`codesession::peer_claims` selects `room = ? AND session <> viewer`). So
  for CC the claim rides the `UserPromptSubmit` `additionalContext` system-reminder
  layer; for codex + driven CC resume it rides `build_resume_message` (prepended
  ahead of the delivered message, out of band, after the cached prefix). The block
  is presented as advisory ("route around these files, nothing is locked") and
  capped at 50 lines so a busy room can't flood a turn.

- **Crash-release (lease-style, docs/topics.md decided-5).** Claims/membership are
  NOT auto-released on a one-shot turn-process exit — a coding session is DURABLE and
  RESUMABLE (M2-A: a turn ending is not the session ending), so a worker's claims
  persist between turns or it would lose them the instant it finished a turn.
  Release is by (1) explicit `elanus code unclaim`, and (2) crash-reap:
  `reap_dead_members` (new) drops the membership + claims of any session whose
  recorded `owner_pid` is dead — a signal-0 liveness probe, EPERM-treated-as-alive,
  exactly mirroring the session-token `reap_orphans`. Run at daemon boot AND launcher
  boot (the same liveness sweep as the credential reaper), so a SIGKILL'd (or
  finished) session's claims don't linger in roommates' injections forever.

- **No token widening (preserves the verified property).** Rooms/claims are
  kernel-side ledger SQL gated by the session's env-derived identity, exactly the M3
  approach — the emit-only bus token is **structurally unchanged**
  (`SessionToken.subscribe` stays `Vec::new()`; the session still cannot subscribe to
  anything, including `in/group/<id>`). The advisory `in/group/<id>` room is the
  conceptual address from docs/topics.md; the implementation backs it with the
  ledger (claims a session reads are a SQL query of its room), not a live bus
  subscription — consistent with M3's inbox (a ledger query, not a subscribe).

- **Live evidence (isolated worktree stack).** Two codex sessions in room
  `m5-room`: A = `code-942a7e61`, B = `code-d50f58cd`. A recorded a claim via the
  real `elanus code claim src/foo.rs` CLI path (env identity).
  - **B sees A's claim, own excluded:** B's `code claims` view showed
    `code-942a7e61 is editing src/foo.rs` under peer claims; A's own view listed
    `src/foo.rs` under "your claims" with "no peer claims" — own correctly excluded.
    Bidirectional confirmed (after B claimed `src/b-only.rs`, A's peer view showed it
    while still excluding A's own `src/foo.rs`).
  - **Real codex resume of B with A's claim active (the headline):** a delivery to
    B's mailbox drove a real daemon resume; the per-turn injection
    (`build_resume_message`) carried A's `[elanus peers]` claim, and B's model
    replied **`PEER-IS-EDITING src/foo.rs`** (trace.jsonl, stamped
    `sender=code-d50f58cd`) — proof the advisory reached the model and B routed
    around the file. `cached_input_tokens` rose (48896), confirming the injection
    landed after the cached prefix.
  - **Room isolation:** session `code-cccc0003` in room `m5-other` saw NO peer
    claims — `m5-room`'s `src/foo.rs` is invisible across rooms.
  - **Own-write-only:** B's claim recorded `session=code-d50f58cd` (its env
    identity); the CLI takes only `<path>` (no session arg), so there is no code path
    to forge a claim as another session.
  - **Crash-release:** SIGKILL'd A's live owner process; the next launcher boot
    logged "reaped claims of dead session code-942a7e61 in room m5-room" and the
    claim was gone from `code_claims` — it stopped appearing in B's view.
  - **No regression:** M3 inbox (unread count + preview) + the memory note +
    peer claims all coexist in one `[elanus]` injection (shown live for one session
    with all three); launch/resume/deliver/M4 routing unbroken; emit-only token
    unchanged; `~/.codex` config untouched (`config.toml` Jun 19, `auth.json` Jun 16
    predate the run); no idle credential after.

- **Honest residual.** Crash-reap of a session that finished a turn (its launcher pid
  is dead) happens at the NEXT launcher/daemon boot, not the instant the process
  exits — between an exit and the next boot, a finished session's claims linger
  (advisory, eventually-consistent, the same boot-only cadence as `reap_orphans`).
  This matches the resumable model (a finished turn is not the session ending). An
  explicit `unclaim` frees a path immediately. The room is backed by the ledger, not
  a live `in/group/<id>` subscription — a true live room subscription is a later
  step if real-time (not per-turn) claim propagation is needed.

- **Tests.** `cargo test` green, **159 passing** (was 148): +6 codesession
  (`claim_round_trips_and_peer_view_excludes_own` — the crux; `rooms_are_isolated_no_cross_room_claim_leak`;
  `own_write_only_a_session_cannot_forge_a_claim_as_another`;
  `unclaim_releases_a_path_from_peers_view`;
  `reap_dead_members_releases_a_sigkilled_sessions_claims_keeps_live`;
  `upsert_preserves_a_room_set_at_launch`), +5 codeagent
  (`take_room_flag_extracts_and_strips_room`;
  `turn_injection_surfaces_peer_claims_excluding_own`;
  `turn_injection_room_isolation_no_cross_room_claims`;
  `build_resume_message_carries_peer_claims_to_a_driven_turn`;
  `solo_session_has_no_peers_and_no_claim_injection`).

### Still TODO (next increments)
- ~~**M5 — peer coordination over the bus (advisory).**~~ **DONE 2026-06-20** (see
  the M5 Log entry above). A coordination room (`--room <id>`, ledger-backed),
  `elanus code claim`/`unclaim`/`claims` (env-derived identity), surfaced in each
  session's per-turn M3 injection as an `[elanus peers]` block excluding its own,
  crash-released by `reap_dead_members` (owner-pid liveness, like `reap_orphans`).
  The emit-only token is unchanged (rooms/claims are a kernel-side ledger query, not
  a bus subscribe). **This completes M0–M5.** Residual: the live `in/group/<id>`
  room subscription (real-time vs per-turn propagation) and block-substrate
  integration (below) are the natural next steps, not needed for the advisory surface.
- **Block-substrate integration (deferred from M3).** Fold the memory note and the
  inbox status into `context_blocks` (the note as a `Session`-scoped block, the
  inbox status as a computed block with a build-log entry) if/when M5's claims block
  makes the substrate reuse clean. Today they are a thin stored value + an inline
  computed string.
- ~~M3 session read grant.~~ **DONE 2026-06-20** (see the M3 Log entry above). The
  read was NOT done by widening the bus token's `subscribe` scope — that stays empty
  (emit-only) — but by a kernel-side scoped LEDGER QUERY gated by the session's own
  env-derived identity (`codesession::inbox_for_session`). So the M2-A "no read
  authority on the bus token" property is preserved; the read is own-inbox-only by
  construction.

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
