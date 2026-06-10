# Minimal Agent Harness — Design Handoff

> Status: design phase, pre-implementation. This doc captures architectural decisions
> from design discussions (June 2026: initial claude.ai session, revised in a
> follow-up). Decisions marked **[DECIDED]** are settled, rationale included so you
> can challenge them if implementation reveals problems. Items marked **[OPEN]** need
> a decision before or during implementation.
> Author context: Tim has prior art in open-strix (Discord agent, self-written skills,
> untrusted input, prompt injection as a live threat model). This harness is partly a
> reaction to lessons learned there.

## Thesis

**[DECIDED]** The harness is `inetd + cron + git hooks + sqlite + a flight recorder`.
All invocations are events. The kernel is an event log, a trace log, a dispatcher,
and two narrow contracts (handler execution, render provider) — a few hundred lines.
Everything else (indexing, memory, workflows, channel adapters, human interaction,
algedonic monitors) is userland: **skill packages** containing executables.

The size discipline test for any proposed kernel feature: **can it be a handler?
Then it's a handler.** The harness gets smaller by refusing to contain things.

## Non-goals

- No workflow engine. A workflow is a script that calls `harness exec`. The OS is
  the workflow engine. RLM-style dynamic workflows live in userland as scripts that
  spawn child execs with restricted context views.
- No personality. Self-mutating memory blocks are **punted, not designed for** — if
  ever revisited, they're pure userland (a kv table + an agent write tool + a render
  provider; zero kernel support), so punting costs nothing. The open-strix lesson
  stands: mutable free-text the agent edits about itself creates self-reinforcing
  drift — same failure mode as ambient-retrieval slop loops.
- No per-tool-call approvals. Approvals are capability grants (sandbox policy
  changes), not actions.
- No built-in UI. State lives in sqlite precisely so any UI (datasette, custom web,
  TUI) attaches as an external dependency.
- No second bus. Signals, asks, ticks, messages — everything rides the one event
  log. What differs is dispatch *policy*, never machinery (see Signals).

## Kernel

### 1. Event table (sqlite, WAL mode)

The coordination substrate. State is derived from the log; crash recovery is replay.
Note the division of labor: this table is the **work queue** (it has mutable state,
the dispatcher queries it); it is *not* the debug log — that's the trace (next
section). Illustrative schema — adjust freely:

```sql
CREATE TABLE events (
  id             INTEGER PRIMARY KEY,
  type           TEXT NOT NULL,            -- 'cron.tick', 'discord.message', 'human.ask', 'signal.pain', ...
  cause_id       INTEGER REFERENCES events(id),  -- causality chain (see Events section)
  correlation_id TEXT,                     -- ties asks to answers, requests to resumes
  payload        TEXT,                     -- JSON
  state          TEXT NOT NULL DEFAULT 'pending',
                 -- pending | running | done | failed | waiting_on_human | expired
  priority       INTEGER DEFAULT 0,
  deadline       TEXT,                     -- ISO8601; for human.ask: when default fires
  default_action TEXT,                     -- JSON; what happens if deadline passes unanswered
  idempotency_key TEXT,
  created_at     TEXT DEFAULT (datetime('now')),
  finished_at    TEXT
);
CREATE INDEX idx_events_pending ON events(state, type, priority);
CREATE INDEX idx_events_correlation ON events(correlation_id);
```

Delivery semantics **[DECIDED]**: at-least-once + idempotent handlers
(idempotency_key). Do not attempt exactly-once.

### 2. Trace log (`trace.jsonl`)

**[DECIDED]** An append-only JSONL flight recorder that ties together everything
happening in the agent — all threads, one stream. It exists for debugging and
reconstruction; it must be possible to re-create what happened from it alone.

Properties:

- **Append-only, write-only.** Nothing in the system ever reads the trace for
  control flow. (State lives in the events table.) This property is what makes the
  trace safe to rotate, truncate, tail, or ship elsewhere.
- **Self-contained.** Dispatch info is duplicated into the trace rather than
  requiring a join against sqlite. A debugging artifact should stand alone; disk is
  cheap.
- One JSON object per line, each line a single `write()` with `O_APPEND` so
  multi-process interleaving is safe.

Line shape: `{ts, event_id, cause_id, correlation_id, session_id, kind, payload}`.
Kinds: `dispatch`, `handler.start`, `handler.exit`, `llm.request`, `llm.response`,
`tool.call`, `tool.result`, `emit`, `signal`.

- **Tool calls are the heart of the trace — tools are the truth.** Log `tool.call`
  *before* execution and `tool.result` after, so a crash mid-tool is visible as a
  call with no result. Both are indispensable for reconstruction.
- **[DECIDED]** Thinking blocks are *excluded* from the trace — they're not
  evidence. They're preserved in full in the transcript (see Sessions), so nothing
  is lost; the trace records what the agent *did*, not what it mused.

Three writers cover everything: the dispatcher (dispatch, handler exits), the exec
handler's tool loop (LLM requests/responses, tool calls/results — the loop is
hand-rolled precisely so provenance capture like this is owned code, see Stack), and
`harness trace` for arbitrary handlers.

**[OPEN]** Rotation/retention policy (daily files? size-based? never, until it
hurts?).

### 3. Sessions / transcripts

**[DECIDED]** Conversations live in sqlite, one row per message — queryable, and
ring-buffer views fall out for free. The transcript is where thinking blocks,
full message content, and tool results live in full fidelity; it is also the
process state that makes checkpoint/resume nearly free (see Human Actor). Sketch:

```sql
CREATE TABLE messages (
  id         INTEGER PRIMARY KEY,
  session_id TEXT NOT NULL,
  role       TEXT NOT NULL,     -- user | assistant | tool
  content    TEXT,              -- JSON; includes thinking blocks, tool results
  event_id   INTEGER REFERENCES events(id),  -- the exec turn that produced it
  created_at TEXT DEFAULT (datetime('now'))
);
CREATE INDEX idx_messages_session ON messages(session_id, id);
```

### 4. Dispatcher daemon

Does *nothing* but: notice pending events → match type to handler(s) → check
throttle table → fork/exec handler → record exit + any emitted events → write trace
lines. Stateless, restartable, let-it-crash. It is a supervisor, not a doer. **It
does not index, it does not call LLMs, it does not talk to Discord.** All of that is
handlers.

**[OPEN]** Wakeup mechanism: simple polling interval vs sqlite update_hook vs
watching the WAL. Start with polling (1s is fine for a personal harness); optimize
only if it hurts.

### 5. Handler execution contract

How an individual handler executable is invoked (unchanged regardless of how it was
registered — see Skill packages for registration):

- Event JSON on stdin.
- Env: `HARNESS_EVENT_ID`, `HARNESS_CAUSE_ID`, `HARNESS_CORRELATION_ID`,
  `HARNESS_DB` (path), `HARNESS_TRACE` (path), `HARNESS_PROFILE` (path).
- Exit 0 = done. Nonzero = failed (dispatcher records; retry policy per event type).
- A distinguished exit code (e.g. 75, à la EX_TEMPFAIL) = **suspended** —
  handler checkpointed itself and exited; resume happens via correlation_id
  (see Human Actor section).
- Handlers emit new events via `harness emit`, which reads `HARNESS_EVENT_ID` from
  env and threads `cause_id` automatically. Causality propagation must be
  zero-effort or it won't happen.

## Skill packages

**[DECIDED]** The unit of functionality is a **skill package**: a directory
conforming to the [agentskills.io](https://agentskills.io) standard, optionally
extended with a harness manifest. This replaces the earlier design where
`handlers.d/` (organized by event type) was the source of truth — that smeared one
capability across many directories; a Discord adapter would have been a producer
daemon + a reply handler + an indexer + a cron entry in four places. Packages bundle
by capability, and because the unit *is* a skill, self-containment doesn't cost
discoverability — it's naturally on the "path."

```
skills/discord/
  SKILL.md           # agent-facing instructions, agentskills.io-compliant
  harness.toml       # harness-facing manifest: handlers, cron, throttles
  scripts/
    reply            # executables, any language
    index-message
```

Division of audiences, kept strictly separate:

- **`SKILL.md` stays pure per the spec** (frontmatter `name`/`description` + body).
  Any vanilla skill consumer can use it. The spec's frontmatter `metadata` field is
  string→string only, so structured registration does *not* go there.
- **`harness.toml`** is a sibling file (the spec explicitly permits additional
  files) carrying everything the dispatcher needs:

```toml
[[handler]]
on    = "discord.message"   # event type, glob ok
run   = "scripts/reply"
order = 0                   # cross-package ordering, replaces NN- prefixes

[[handler]]
on    = "discord.message"
run   = "scripts/index-message"
order = 50

[[cron]]
schedule = "*/5 * * * *"
emit     = "feeds.check"

[throttle."discord.*"]
max_concurrent = 2
```

- A pure indexer with no agent-facing instructions is a package with a manifest and
  no (or stub) SKILL.md. A pure instruction skill has no manifest. Many packages
  have both (the Discord package: handlers for inbound messages *and* a SKILL.md
  telling the agent how to send them). Don't force ceremony either way.

**Registration**: `harness enable <skill>` reads the manifest and materializes
`handlers.d/<event.type>/NN-name` symlinks, systemd-unit style. The manifest is the
source of truth; `handlers.d/` is the compiled routing table. The dispatcher's logic
stays "scan a directory," and debugging stays `ls`. **[OPEN]** Whether to keep the
materialized form or have the dispatcher read manifests directly — materialized is
dumber, which is a virtue here.

**Scope wrinkle [DECIDED, revisit if it chafes]**: handler registration is
harness-global (an indexer indexes regardless of profile; the daemon serves the
whole harness). SKILL.md *visibility* is per-profile — profiles select which skills
render into context. One package, two facets, two activation scopes.

## Primitives (CLI)

**[DECIDED]** Two runtime verbs:

- `harness emit <type> [--payload ...] [--priority ...] [--deadline ... --default ...]`
  — universal entry point. Cron ticks, webhooks, CLI invocations, agent-spawned
  work, signals: all just emit.
- `harness exec [--session ID] [--profile PATH] <prompt|->`
  — run an agent turn. **Chat is exec with a session ID** — one primitive, not two.

Everything else is either sugar over emit (`harness ask`, `harness answer`) or
plumbing, not semantics (`harness trace` appends a trace line; `harness enable`
materializes a package's registrations).

## Events: causality, workload typing, throttling

**[DECIDED]** Every event carries `cause_id`. The chain propagates into LLM request
metadata. This buys, as queries over the log:

- Throttling by event type *at the request level* (the original requirement).
- Different throttle policies for agent-initiated vs human-initiated work.
- Cost attribution per event type / per root cause.
- Full audit trail: any action traces back to the webhook / cron tick / message
  that spawned it. For an agent that writes its own skills, the causality chain
  **is** the audit log.

Throttle table sketch:

```sql
CREATE TABLE throttles (
  event_type   TEXT PRIMARY KEY,   -- supports glob, e.g. 'agent.*', 'signal.*'
  max_concurrent INTEGER,
  rate_per_min   INTEGER,
  llm_tokens_per_hour INTEGER,     -- enforced by the exec handler, read from here
  coalesce       INTEGER DEFAULT 1 -- 0 for signal.*: never batch, never queue behind
);
```

Note: **the human gets a row in this table** (see Human Actor — interrupt
coalescing is just throttling).

## Signals (the algedonic channel)

**[DECIDED]** No second bus. A signal is an event (`signal.pain`,
`signal.anomaly`, ...); a hook is just a handler subscribed to `signal.*`. What
makes the channel *algedonic* — Beer's bypass-the-hierarchy property — is **dispatch
policy, not machinery**: the `signal.*` class in the throttle table is exempt from
coalescing, never queues behind other work, and punches through the human-proxy's
digest batching.

The agent is on both ends of the channel, via the same `harness emit`:

- **Consuming**: signals wake or interrupt execs. Preemption of in-flight work is
  the one genuinely new behavior: a running exec checks for pending `signal.*`
  events *between tool calls*. That's a tool-loop concern — it lives in the exec
  handler, userland, not the kernel.
- **Emitting**, from two sources worth distinguishing:
  - *Self-reported* pain: the agent has an emit tool and decides to scream.
  - *Measured* pain: monitor handlers watching the events table and trace (error
    rates, cost burn, loop detection) emit on the agent's behalf. Measured pain is
    more trustworthy, for the same reason tools are the truth. Build both; never
    rely on only the first.

**[OPEN]** Signal taxonomy (what lives under `signal.*`; severity levels) and
preemption granularity.

## Indexing

**[DECIDED]** Indexers are handlers consuming `file.changed` / `message.received` /
etc. events — *not* daemon responsibilities — packaged as skill packages. "Many
kinds of indexing" = many packages subscribed to the same stream, independently
crashable, independently throttled. Indexers write derived tables (or external
stores like Qdrant) keyed back to source events/files for provenance.

## Memory

**[DECIDED]** The kernel knows nothing about memory. A memory system is any
userland package that plugs into two seams:

1. **Write seam** — handlers consuming events to build whatever derived store they
   want: sqlite tables, FTS5, Qdrant, a graph, KB files in git.
2. **Read seam** — render providers the context assembler invokes at prompt time
   (see Context assembly).

SQL views are the *trivial implementation* of this contract, not the architecture:
ring buffer = `SELECT ... ORDER BY id DESC LIMIT n`; registers = rows in a kv
table; computed registers (time of day, presence, git status) = functions evaluated
at render time, good *because* derived — they can't drift. Use them when they fit.
But a memory system that never touches sqlite is equally first-class; the two seams
are the whole interface. This is what keeps memory decoupled: anyone can build any
memory system out of the substrate.

Other settled pieces:

- **[DECIDED]** Durable knowledge is KB-in-git: append-mostly files, diffable,
  revertible, line-level provenance, review gates on agent-proposed changes (a
  "memory write" is a commit the human can review or auto-merge per policy).
- Issue tracker = a table + status-transition events. Externalized, auditable,
  lifecycle-bearing memory. A userland package, not a subsystem.
- Self-mutating memory blocks: punted (see Non-goals). The seams make them
  expressible later without kernel changes, which is exactly why punting is safe.

## Context assembly

The one real kernel-adjacent component: a render step that turns (render-provider
outputs + KB excerpts + skills) into the prompt's context blocks, per profile.
Blocks exist only as a *render target*, never as a mutable store.

**[OPEN]** Render-provider contract shape — lean: an executable declared in a
package's manifest, invoked at render time with (profile, session, query hints) on
stdin, returning block content on stdout. **[OPEN]** Whether assembly is a library
linked into the exec handler or a standalone `harness render` the exec handler
shells out to. Lean standalone — inspectable with `| less`, testable in isolation.

## Profiles

**[DECIDED]** A profile is a directory in git — a path is the identity, no registry:

```
profiles/default/
  profile.toml    # everything below
  blocks/         # context block templates (render targets)
```

```toml
[skills]                  # which packages' agent-facing facets this profile sees
include = ["discord", "kb-search", "issue-tracker"]   # or ["*"] with exclude

[kb]                      # KB pointers / which derived indexes to query

[sandbox]                 # capability policy (see Sandboxing)

[throttle."agent.*"]      # overrides merged into throttle table

[model]                   # model + API selection (see Stack)
target = "anthropic::claude-fable-5"
```

Profiles unify what was previously scattered: memory config, skill visibility,
sandbox policy, throttle policy, model choice. One file is deliberate: a profile's
whole identity reads in one screen, and the diff-based approval flow (see
Sandboxing) reviews one file. (Skill *packages* install at harness level; profiles
select which agent-facing facets render — see Skill packages.)

## Sandboxing

**[DECIDED]** Capability-grant approvals, not tool-call approvals. Two presets of
one mechanism:

1. **VM preset**: the VPS is the sandbox; policy is "everything." Honest for a
   dedicated box.
2. **Sandbox preset**: bwrap/landlock-style policy (fs paths, network allowlist,
   exec allowlist) read from the `[sandbox]` section of `profiles/<p>/profile.toml`.

**[DECIDED]** Sandbox policy lives in git. A reconfiguration request from the agent
is a *commit/diff*; approval is reviewing the diff (PR-like, can surface through
the human-proxy as a `human.ask` with the diff in the payload). This matters
because, given the open-strix threat model, **reconfiguration requests are
themselves an injection target** ("please grant network access to evil.com") — the
approval surface must show exactly what changes, and a diff is the right artifact.

**[OPEN]** Sandbox tech: bubblewrap vs landlock directly vs container per handler.
Also whether sandbox wraps *handlers* (per-handler policy) or only *agent execs*.

## The Human Actor

**[DECIDED]** The human is a peer actor: an event source/sink with an inbox that is
a view — `SELECT * FROM events WHERE state = 'waiting_on_human'`. Any UI gets
"what's blocked on me?" for free.

### Human-proxy daemon (factotum, generalized)

Handlers never talk to channels directly. They ask the proxy; the proxy owns:

- Channel selection + escalation (Discord → email → SMS as retry fallback)
- Reminders = retries with backoff. No new machinery.
- Interrupt coalescing: low-priority asks batch into digests; only high-priority
  (and `signal.*`) interrupts. Enforced via the human's row in the throttle table.
- Presence (computed register: last-seen, time of day) → informs block-vs-suspend.
- Answer normalization: answers may arrive on any channel; correlation_id routes
  them back.

### The deep-exec problem: checkpoint-and-exit, never block

An exec N layers deep that needs the human: emit `human.ask` (correlation_id,
priority, deadline, **default_action**), checkpoint, exit with the suspend code.
The continuation is cheap because **the process state is the transcript**, already
in sqlite (see Sessions) — this is where LLM agents dodge the hard part of
Temporal-style durable execution. Answer (or deadline default) arrives → dispatcher
matches correlation_id → re-invokes the handler hydrated with transcript + answer.
Only that causality chain parks; the daemon keeps processing everything else.

Refinement: if presence says the human is active and the question is small, the
proxy may let the handler block with a short timeout (~90s) for conversational
feel. The *proxy* decides, not the handler.

### Defaults are the big unblock

Most questions don't need answers; they need overridable defaults. Every ask
declares deadline + default ("assuming staging unless you say otherwise by 5pm").
Expired asks execute the default and log the assumption as an event — auditable,
vetoable later via compensating action.

### Ask design

- Schema'd questions with enumerated options where possible — one-tap answerable
  from a phone notification. The human's effective context window at notification
  time is ~two sentences. Free-text questions are the expensive path.
- Asks carry their causality chain so the human can see *why* they're being asked.

### Staleness

The checkpoint records what the question's answer depends on (file hashes, event
watermark). On resume, validate before acting on a three-day-old "yes." On
mismatch: re-ask or recompute. **Silently proceeding on a stale answer is the bug.**

## Stack notes

**[OPEN — leaning Rust]** If Rust:

- Provider layer: **genai** (jeremychone/rust-genai). Rationale from prior
  research: models Chat Completions and the Responses API as *separate adapters*
  (`openai::` vs `openai_resp::` namespacing), native Anthropic adapter (reasoning
  effort, cache_control) rather than an OpenAI-compat shim, and
  `ServiceTargetResolver` for custom endpoints/auth (Azure/Foundry). Model + API
  selection at runtime via one config string: `adapter::model-name` — store in
  the `[model]` section of `profiles/<p>/profile.toml`. Caveat: pre-1.0, APIs
  churn (resolver went async;
  `ModelIden::with_name_or_clone` → `from_option_name`); pin the version.
- genai does NOT provide the tool loop — by design. Write the ~30-line loop in the
  exec handler. **This is correct for this project**: termination policy, tool
  allowlisting, injection checks before dispatch, signal-preemption checks between
  tool calls, trace capture (`llm.*`, `tool.*` lines), and provenance all live in
  that loop and must be owned, not inherited. (If a prebuilt loop is ever wanted:
  rig for ergonomics, synaptic for LangGraph-shaped graphs, AutoAgents for its
  WASM untrusted-tool sandbox. But default is hand-rolled.)
- Handlers are language-agnostic by contract regardless — only the kernel and the
  exec handler need to share a language.

Alternative **[OPEN]**: kernel in Python/TS for iteration speed, port later. The
handler contract makes the kernel swappable; the schema is the real interface.

## Open questions (consolidated)

1. Dispatcher wakeup: poll vs update_hook vs WAL watch. (Start: poll.)
2. Context assembly: library vs `harness render` subprocess (lean: subprocess), and
   the render-provider contract shape (lean: manifest-declared executable, stdin →
   stdout).
3. Sandbox tech (bwrap / landlock / container) and scope (handlers vs execs only).
4. Kernel language: Rust + genai vs Python first.
5. Retry policy representation: per event type in throttles table vs separate table.
6. Trace rotation/retention policy.
7. Registration: keep materialized `handlers.d/` (via `harness enable`) vs
   dispatcher reads manifests directly. (Lean: materialized — dumber dispatcher.)
8. Signal taxonomy under `signal.*` and preemption granularity.
9. How RLM-style context isolation maps: child execs with restricted profile/views.
   Userland, but the profile mechanism should make it expressible early.
10. KB indexing targets: sqlite FTS5 first? Qdrant/ColBERT later as another package?

## Suggested build order

1. **Kernel skeleton**: events table + `trace.jsonl` + `harness emit`/`harness
   trace` + dispatcher (poll, fork/exec, record) + handler execution contract.
   First producer: cron tick. First handler: echo. Prove causality threading via
   env *and* that the trace alone reconstructs the run.
2. **Skill packaging**: `harness.toml` manifest + `harness enable` materialization.
   The echo handler becomes the first package.
3. **Throttles**: table + dispatcher enforcement + per-type concurrency. Verify
   agent-vs-human-initiated policies work via cause-chain root inspection.
4. **Exec handler**: genai (or chosen layer) + hand-rolled tool loop + messages
   table. `harness exec --session` = chat. Request metadata carries event
   type/causality; tool loop writes `llm.*` / `tool.call` / `tool.result` trace
   lines.
5. **Human-proxy**: `human.ask`/`human.answer`, Discord adapter as a skill package
   (producer + consumer), deadline/default expiry, suspend-exit-code + resume via
   correlation_id. This is the riskiest novel machinery — do it before it
   calcifies around assumptions.
6. **Context assembly**: render providers, computed registers, `harness render`,
   first profile directory.
7. **Indexing packages** + KB-in-git flow (memory writes as commits).
8. **Signals**: `signal.*` throttle class (no-coalesce), first measured-pain
   monitor (cost-burn watcher over the trace/events), preemption check in the tool
   loop.
9. **Sandbox policies** + diff-based reconfiguration approvals through the proxy.

Milestone test after step 5: a cron tick wakes the agent, it works for a while,
hits a question, suspends, pings Discord, you answer from your phone with one tap,
it resumes and finishes — `SELECT` on the events table shows the entire causality
chain, and `trace.jsonl` replays the entire run, tool call by tool call.
