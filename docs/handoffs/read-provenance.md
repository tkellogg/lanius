---
status: in-progress — M1 (advisory read camera) + M3 (legibility/fast-fail subscribe) shipped; M2 (authoritative cage camera) deferred, Linux seccomp / macOS-ES gated
author: Claude Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-23
---

# Handoff: read provenance — make "what did this agent read" a subscription

Make the files an agent *reads* as observable as the files it *writes*, so the
question the human in [../journeys/10-what-did-the-agent-read.md](../journeys/10-what-did-the-agent-read.md)
couldn't answer — **"what did you read that told you to do that?"** — becomes a
query against the bus, not an archaeology dig. Reads are the injection half of the
threat model and today they are the one thing elanus does not witness.

Answers Tim's "detecting files read" item in [../_questions.md](../_questions.md),
and builds the `[OPEN — leaning]` **"The read camera"** section of
[../sandbox.md](../sandbox.md) — which has already settled the doctrine this
handoff implements.

## Read the wonky bits first (decisions to confirm)

Five things surfaced while gaming this out. They change the shape of the work, so
confirm them before committing:

1. **The `_questions.md` deny→catch→allow→retry sketch is dead for *two*
   independent reasons.** Tim's own doubt — *"I'm not sure we're actually in the
   path to catching these OS errors"* — is correct, and there's a second, deeper
   problem:
   - **Not in the path.** The cage is a **static** Seatbelt/Landlock profile
     (`src/sandbox.rs`): the kernel denies the `openat` and the *agent* gets
     `EACCES`; elanus gets **no callback**. Nothing to hang an event on.
   - **Even with a callback, you can't transparently un-fail it.** The `cat`
     already received `EACCES` and returned — there is no "quick-change to succeed,"
     because the agent's process will not re-issue the syscall. To make a read both
     *pause* and *then succeed* you must hold the syscall **mid-flight** — which is
     exactly what seccomp user-notification / Endpoint Security / fanotify-permission
     do. So the trick doesn't avoid those mechanisms; it **collapses into** them
     (slower and racier). This handoff drops it and adopts `sandbox.md`'s
     allow-and-notify framing, which `sandbox.md` already settled on.

2. **The tool-call read stream already exists — and it is *not* what's wanted.**
   Claude Code's generated hook config (`claude_settings` in `src/codeagent.rs`)
   wires `PreToolUse`/`PostToolUse` with matcher `"*"` — every tool, including
   `Read`/`Grep`/`Glob` — so those reads already publish as
   `obs/agent/<name>/<session>/tool/Read/{call,result}`. But this stream is
   **advisory, not authoritative**: it records what an *honest* agent's Read tool
   did, and a `Bash`-plus-`cat` (or any tool that opens a file without going through
   the Read tool) walks straight around it. The ask is the **authoritative,
   can't-be-bypassed** version. So M1 (project this stream into a path-keyed view)
   is a real but *partial* win — useful against an honest agent, useless against a
   hostile one — and the heart of this handoff is **M2**, which sits below the
   shell. Do not mistake M1 for the answer.

3. **A cage-level read camera buys ~nothing for coding agents *today*** — the very
   actors the journey is about. Coding agents are **not** inside elanus's cage:
   `src/codeagent.rs:52-61` keeps each tool's *own* sandbox active because bypassing
   onto today's write-only cage would be a containment regression; the bypass is a
   **deferred milestone** in [coding-agents.md](coding-agents.md). A syscall read
   camera only sees processes elanus cages, so for a Claude/Codex session it sees
   nothing until that bypass lands. The cage camera (M2) pays off for **kernel
   shell/exec tool calls and package actors** (which *are* caged) first; its value
   for coding agents is gated on coding-agents.md.

4. **macOS is the hard platform and it's Tim's machine.** Seatbelt *enforces* read
   denials for free but **emits nothing** on allowed reads. Observation needs
   Endpoint Security (`ES_EVENT_TYPE_NOTIFY_OPEN`) — a **signed system extension
   with an entitlement**, far heavier than `sandbox-exec`. So the unprivileged
   seccomp-unotify path is **Linux-only**; on the current Mac the cage read camera
   (M2) is an accepted gap until ES or the Linux/VPS move. Net: **M1 is the only
   tier that delivers on macOS now.**

5. **Three read sources, three fates — be honest about the residual.** "What did
   the agent read" is not one thing:
   - **(A) explicit tool reads** (`Read`/`Grep`/`Glob`) — captured now (M1).
   - **(B) shell-buried reads** (`cat`, `grep`, `<` redirects, a build's source
     inputs — anything inside a `Bash` tool call) — invisible to M1 (we see the
     command string, not the opens) and the *only* real answer is the cage camera
     (M2), and only once the agent is caged (M3).
   - **(C) context auto-loads** (CLAUDE.md, MCP resources, injected system
     reminders) — read by the *harness*, not the agent process in a way either tier
     sees. Probably an accepted gap; name it, don't pretend to close it.

   The journey's actual incident — an instruction "buried in a doc I read" — is
   most likely **(A)** (so M1 would have answered it) or **(B)** (so it needed M2 +
   caging). Worth confirming which, because it tells us whether M1 alone closes the
   journey.

## Doctrine (settled in sandbox.md — do not relitigate)

From [../sandbox.md](../sandbox.md) "The read camera":

- **Per-open, not per-read.** One `cat` is one `openat` and many `read`s; a build
  issues millions of reads. The event is the **open**.
- **Allow-and-notify, not allow-or-veto, to start.** Build the *camera* (observe);
  the same interception point can later host a blocking read *hook* (veto a read of
  a secret) — the camera/hook split mirrored on the read side.
- **Volume is opt-in, like the write camera.** A read flavor on the `obs/fs` noun
  defaults to recorder `none` (`src/recorder.rs` already defaults `obs/fs/#` to
  `Sink::None`); you subscribe the subtree you care about. An unscoped firehose
  drowns everything.
- **Read *enforcement* rides the cage and is independent of all this** — the read
  *scope* (deny-listed secrets) is always-on once the read envelope lands; only the
  *camera* is optional.

## How the write camera works today (the model to mirror)

The write camera is a **boundary diff**, not a live watcher (`src/exec.rs`):

- `sandbox::snapshot(cage)` stat-walks the writable roots **before** a tool call
  (`exec.rs:1323`), `snapshot` again **after** (`exec.rs:1325`), `sandbox::diff`
  yields per-file `create|modify|unlink`.
- `emit_fs_delta` (`exec.rs:1447`) publishes one event per change to
  `obs/fs/<encoded-canonical-path>` (`crate::topic::encode_path`), carrying the
  causing `tool_use` id as `cause` — **attribution is structural, not inferred**,
  because the dispatcher already brackets every tool call.

Reads **cannot** reuse this: a read leaves no durable trace to diff. So read
observation is a genuinely different mechanism — either **(M1)** lift it from the
events we already capture, or **(M2)** intercept at the syscall boundary live.

## Milestones

### M1 — Read provenance from the tool stream (the *bypassable* tier you already have)
**This is advisory, not authoritative — see wonky bit #2.** It records an honest
agent's Read/Grep/Glob tool calls; it does not catch a `Bash`+`cat`. Ship it as a
cheap *coordination/honest-agent* convenience, not as the safety boundary. The
authoritative version is M2.

Project the read-shaped tool events elanus **already** publishes
(`obs/agent/<sess>/tool/{Read,Grep,Glob}/call`) into the same spatial,
path-keyed shape as the write camera: emit (or re-project) an `obs/fs/<path>`
event with `op: read` and `via: tool`, carrying the session/dispatch and the
causing `tool_use` id. Then "what did this agent read, and when" is the same
`obs/fs/<subtree>/#` subscription the write side already affords — not a
tool-noun scan, and not the agent's fuzzy memory.

- Recorder default for the read flavor is `none` (opt-in per subtree), matching
  the write camera and `sandbox.md`.
- **Honest scope, logged in the delta:** Claude Code only (Codex's `exec --json`
  stream is not known to surface per-file reads — verify; if not, Codex falls to
  M2). Covers source **(A)** only; **(B)** shell-buried and **(C)** context
  auto-loads are explicitly out of scope here and noted on the event/stream so a
  consumer never reads an empty result as "no reads happened."

**Acceptance:** after a Claude Code session reads files via its Read/Grep/Glob
tools, `obs/fs/<repo-subtree>/#` (read flavor) yields the exact files, timestamps,
and causing `tool_use` id — with no Endpoint Security, no seccomp, no caging of the
agent. Replays the journey: the human pulls the read stream for the session,
scrubbed to the bad turn, and the poisoned file has a return address.

### M2 — The authoritative cage read camera (sits below the shell), Linux-first
A live interception point on the **caged** spawn that emits `obs/fs/<path>`
`op: read` per `openat` and then **allows** the open (camera, not hook). This is
the only thing that catches source **(B)** — reads buried in a shell tool call and
anything the process subtree opens — because it sits **below the shell**, at the
syscall boundary, where `cat` cannot route around it.

> **The decision this milestone forces: authoritative + macOS + no-root/no-entitlement
> — pick two.** Authoritative read capture is intrinsically a syscall/FS-boundary
> problem; every mechanism that delivers it costs root, an entitlement, a userspace
> round-trip, or a user-approved system extension. There is no free, unprivileged,
> macOS-native option. The least-pain authoritative choice **is platform-bound**:

- **Linux, unprivileged — the sweet spot.** seccomp user-notification
  (`SECCOMP_USER_NOTIF`): a supervisor fd receives `openat`/`openat2`/`open`
  notifications with the path, emits, allows. The one box that is **authoritative
  *and* needs no root** — matches elanus's no-root baseline. Cost: each open blocks
  on a userspace round-trip (synchronous), so it **must** ride the opt-in/exclusion
  machinery (`Cage.exclude`) — a build must not pay it on `target/`. Caveat:
  `io_uring` can open files without these syscalls (the known seccomp bypass), so
  the cage should disallow `io_uring` for the capture to stay authoritative.
- **Linux, privileged:** fanotify (`FAN_OPEN`) or an eBPF `openat` tracepoint —
  asynchronous and cheaper per event, but need `CAP_SYS_ADMIN`. An upgrade path if
  the unprivileged round-trip ever measures too slow.
- **macOS — no free authoritative option (this is the hard platform, and it's the
  dev machine):**
  - *Endpoint Security* (`ES_EVENT_TYPE_NOTIFY_OPEN`): the supported API, async,
    clean — but needs the endpoint-security **entitlement + a signed system
    extension**. Heavy.
  - *`fs_usage` / DTrace on the pid tree* (`fs_usage -w -f filesys -p <pid>`,
    `syscall::open*:entry`): authoritative, **no signing/entitlement** — but needs
    **root/sudo** and parsing a text stream + following the process tree. A viable
    *hacky interim* for a dev box, explicitly not the end state.
  - **Recommended:** treat macOS as **accepted-gap** initially. The status surface
    (M3) reports "unavailable here"; it does not silently no-op. Revisit with ES (if
    the project signs) or when the Linux/VPS move makes seccomp-unotify the norm.
- Reuses the cage's exclusion patterns and the `obs/fs` topic/encoding so write and
  read events share spatial subscription.

**Acceptance:** a caged `sh -c 'cat /secret/path'` (the kernel's own shell tool, or
a package actor — both already caged via `Cage::shell_command`/`Cage::command`)
emits an `obs/fs` read event for that path with the causing dispatch id, on Linux,
unprivileged, **including when the read is buried inside the shell command** (the
whole point — M1 cannot do this). macOS reports the camera unavailable cleanly
rather than dropping reads on the floor.

### M3 — Legibility: config, status surface, fast-fail subscribe
Per `sandbox.md`'s two hard requirements, three states must be legible —
*available and on*, *available and off*, *unavailable here*:

- **Readable config + status.** Whether the read camera is enabled, and whether it
  is even available on this platform/privilege, shows up plainly in config and on
  the system-status / trust surface (beside root, credential, broker) — not buried.
- **Fast-fail subscribe.** Subscribing to the read-camera topics when it is off or
  unavailable must fail fast and loud — a clear per-filter SUBACK 0x87 / error
  event (the bus already does this, bus.md), **never** a silently-empty
  subscription that reads as "no file reads happened." This is the history-503
  lesson.

**Acceptance:** `elanus status` (or the equivalent trust surface) reports the read
camera's availability + on/off; a subscribe to the read flavor when unavailable
returns a failure the consumer can see, not silence.

## Dependencies / gating (read before sequencing)

- **M1 has no dependencies** — it rides events already on the bus. Do it first; it
  is the only tier that works on macOS today and likely closes the journey's own
  incident.
- **M2's value for coding agents is gated on [coding-agents.md](coding-agents.md)'s
  tool-sandbox bypass** (the milestone that runs Claude/Codex inside elanus's cage
  instead of their own). Until that lands, M2 covers only the kernel's shell/exec
  tool calls and package actors. Build M2 against *those* first; it is still useful
  there, and it is ready for coding agents the moment the bypass lands.
- **M2 on macOS is gated on Endpoint Security** (entitlement/signed extension) or
  the Linux/VPS move. Treat macOS as accepted-gap until then.
- The **read *enforcement*** envelope (deny-listed secret regions) is a separate
  `sandbox.md` workstream (the "read grant is a different shape" note) — this
  handoff is the *camera*, not the scope. They share the interception point M2
  builds but are independently shippable.

## Read these first

- [../journeys/10-what-did-the-agent-read.md](../journeys/10-what-did-the-agent-read.md)
  — the why, first-person.
- [../sandbox.md](../sandbox.md) — "The read camera" (the settled doctrine), "The
  camera: fs events" (the write camera to mirror), "Platform notes" (mechanism
  costs).
- [../../src/sandbox.rs](../../src/sandbox.rs) — `Cage` (write-only Seatbelt,
  macOS-only enforce), `snapshot`/`diff`, `Cage::shell_command`/`command` (the
  caged-spawn entry points M2 hooks).
- [../../src/exec.rs](../../src/exec.rs) — the camera bracket (`snapshot` at 1323,
  `emit_fs_delta` at 1447, `obs/fs/<path>` topic) — the shape M1 mirrors and M2
  extends.
- [../../src/codeagent.rs](../../src/codeagent.rs) — `claude_settings` (PreToolUse
  matcher `"*"` already bridges Read → bus, the basis for M1) and lines 52-61 (why
  coding agents aren't caged yet — the M2/M3 gate).
- [../../src/topic.rs](../../src/topic.rs) — `encode_path`/`encode_segment` (topic
  form M1/M2 reuse).
- [../../src/recorder.rs](../../src/recorder.rs) — `obs/fs/#` defaults to
  `Sink::None` (the opt-in-volume property both tiers inherit).

## Log

- 2026-06-21 — Written from a code read after Tim asked to turn the `_questions.md`
  items into handoffs. Key findings: (1) the deny-retry sketch is a worse
  seccomp-unotify and `sandbox.md` already rejected it; (2) Claude Code's `Read`
  tool calls are *already* on the bus via the `PreToolUse:*` hook, so the cheap win
  (M1) is a projection, not interception; (3) coding agents aren't in elanus's cage
  (`codeagent.rs:52-61`), so a cage read camera does nothing for them until
  coding-agents.md's bypass lands; (4) macOS observation needs Endpoint Security, so
  M1 is the only macOS-viable tier today. Reframed the handoff around three read
  sources (explicit tool / shell-buried / context auto-load) with honest fates.
- 2026-06-21 — Tim **accepted the macOS gap** (M2 option a): build the authoritative
  camera on Linux/seccomp-unotify, sequenced behind coding-agent caging + the
  Linux/VPS move; macOS reports "unavailable" until ES or that move. The
  root-`fs_usage`/DTrace interim is **declined**. M1 stands as the advisory,
  honest-agent tier only — not the safety boundary.
