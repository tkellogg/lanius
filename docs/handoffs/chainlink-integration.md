---
status: proposed
author: Claude Opus 4.8 in Claude Code on Elanus (planner, session c185ae6f)
last-updated: 2026-07-10
---

# Handoff: chainlink integration — durable task state + assignment-as-dispatch

Make `chainlink` (the repo-local sqlite issue tracker, `chainlink` CLI, state in
`.chainlink/`) the **durable task-state layer** for coding-agent work, without
teaching the kernel a single thing about "issues".

Two felt problems drive this:

1. **Interrupted or killed sessions lose work invisibly.** A worker that dies
   (SIGKILL, crash, a closed laptop) leaves no honest record of what it was
   doing or what it touched. The next agent — or Tim — has to reconstruct it.
2. **Assignment should be a handoff primitive between agents.** Handing an issue
   to another agent should *be* dispatch: the assignee wakes and picks it up.

The organizing principle (Tim's brainstorm, ratified): **chainlink is durable
STATE; the lanius bus is live SIGNAL.** Messages stay on the bus. State lives in
chainlink. The integration is a set of thin *bridges* that translate events at
the boundary and keep the two honest against each other. Nothing here is a new
kernel concept — everything is **package + grant + stage + daemon + hook**, the
simple-core law (`docs/channels.md` closing section; the `is_worker_session`
violation in `worker-dm-unification.md` is the anti-pattern we must not repeat).

## Read These First

- The **journey**: this doc's intent section above — durable state vs live
  signal, bridges at the boundary. Reason implementations against it.
- [worker-dm-unification.md](worker-dm-unification.md) — the reference for
  "harness-verified, never payload-claimed" and for keeping taxonomy OUT of the
  kernel. The tombstone (M2) is the same trust move applied to *status*.
- [coding-session-reliability.md](coding-session-reliability.md) — the sibling
  sprint. Its `reap_dead_members` / crash-only reaper discussion is the exact
  machinery M2's kill-9 path rides. chainlink issues #1–#3 already track that
  sprint; this is the state layer under it.
- [read-provenance.md](read-provenance.md) — why a harness observation is
  authoritative where an agent's self-report is not. `inbox_authority` /
  `inbox_block` (`src/codeagent.rs:3288`, `:3333`) is the harness-asserted
  rendering precedent M2 mirrors.
- `docs/context.md:332-391` — the **context stage contract** (M1's stage).
- **Package machinery, cited:**
  - Stage manifest: `[[stage]]` = `StageDecl` (`src/manifest.rs:277-293`);
    exemplars `packages/window/lanius.toml:8-20` (exec + `[[stage.config]]`),
    `kits/memory-blocks-demo/packages/clock-block/lanius.toml:15-25`.
  - Stage runner: `src/context.rs` — `effective_chain` (`:80-125`), `run_stage`
    (`:462-474`), `run_exec_stage` (`:479-526`). The stage reads the whole
    context **document on stdin** and gets `ROOT`/`PACKAGE`/`STAGE` in env; the
    agent noun and session ride *inside the doc* as `doc.meta.agent` /
    `doc.meta.session` (there is **no** session env var — this matters for M1).
  - Daemon manifest: `[process] mode="daemon"` = `ProcessDecl`
    (`src/manifest.rs:192-217`); exemplars `packages/discord/lanius.toml`,
    `packages/recent-history/lanius.toml`. Supervised in `src/dispatcher.rs`
    (spawn `:414+`; per-daemon env `ROOT`/`DB`/`PACKAGE`/`BUS_TOKEN`/`BUS_ADDR`
    at `:496-515`; crash-only).
  - Grants: there is **no "run this binary" grant**. A package that shells an
    external CLI just ships a `run` script (its bytes fold into `code_hash`,
    `src/manifest.rs:653-681`); its authority to act is expressed as scoped
    `subscribe`/`publish` grants in `[request]` (`Request`,
    `src/manifest.rs:170-187`), approved into the ledger
    (`src/packages.rs:302-321`). `provides_builtin_tools`
    (`comms-etiquette/lanius.toml:20`) is the "this package gates a named
    capability" pattern, but is for kernel-defined tools, not an external CLI.
  - The coding-session hook bridge: `lanius code hook <Event>` →
    `codeagent::hook` (`src/main.rs:1657-1659`, `src/codeagent.rs:8898-9145`).
    Stop/StopFailure/SessionEnd → `session/idle` obs + `estimate_retro_once`
    (`:9141-9143`, `:9212-9215`) — the existing "truthful terminal stamp from
    the ledger, once, even if the agent died" precedent.
  - Mailbox + wake: `in/agent/<noun>/<conv>` (`src/codeagent.rs:485`,
    `:519-544`); `lanius code deliver <session> "<msg>"` (`src/main.rs:1684`,
    `codeagent::deliver` `:769-786` → `record_delivery`). The daemon drives a
    pending delivery into the session: `drive_code_deliveries`
    (`src/dispatcher.rs:1395-1441`, SQL prefilter `state='pending' AND type LIKE
    'in/agent/%'`), `recognize_delivery` (`src/codeagent.rs:527-544`),
    `resume_capture`. **This is the wake primitive M3 rides.**
- **Live examples in `.chainlink/`:** issue #4 (worker-dm-unification) shows the
  current *manual* comment/`--kind`/session-work usage patterns. The `--kind`
  vocabulary is real: `chainlink issue comment <id> <text> --kind <k>` where k ∈
  {note, plan, decision, observation, blocker, resolution, result, handoff,
  human} (verified `chainlink issue comment --help`).

## Wonky Bits And Decisions

These are the non-obvious calls. Each has a recommendation; the ones marked
**(confirm)** should get a nod from Tim/Fable before implementation.

1. **chainlink has NO command-triggering hook, and NO `issue assign`
   subcommand.** Verified against the installed binary (`chainlink 0.2.0`):
   `.chainlink/hook-config.json` is *chainlink's own* PreToolUse enforcement
   (git-command blocking + allowed-bash-prefix tracking it injects into Claude
   Code) — it cannot run an arbitrary command when an issue changes. And there
   is no `chainlink issue assign`. **Consequences:** (a) assignment must be
   modeled out of chainlink's existing vocabulary; (b) the wake cannot be a
   chainlink hook — it must be a poller or a lanius verb. This rules out one of
   the three options the scope named.

2. **Assignment = a label `assignee:<noun>` (confirm).** A `locks claim` says
   "*I'm* working on this" (self-directed); a **label** says "*you* should"
   (other-directed) — assignment-between-agents is the latter. Recommend the
   convention `assignee:<noun>` (e.g. `assignee:code-de0c323a` or a stable agent
   noun). `chainlink issue label <id> <label>` is the write; `chainlink export
   -f json` exposes `labels` per issue for the poller (verified: export gives
   `id,title,status,labels,comments,updated_at`). Open: do we also honor `locks
   claim` as a weaker signal? Recommend **no** for MVP — one primitive.

3. **Wake mechanism = a small package daemon that polls, not a kernel verb
   (confirm).** With chainlink hooks ruled out (#1), the honest choices are a
   package daemon polling chainlink, or a `lanius code assign` verb wrapping
   assignment. The **daemon** wins on simple-core: the kernel never learns what
   an "issue" is; the whole integration is one package. It's also durable —
   assignment fires even if the assigner already exited. The daemon polls
   `chainlink export -f json` on an interval, diffs against a watermark of
   already-notified `(issue, assignee)` pairs, and for each *new* assignment
   emits a delivery to the assignee (M3). Cost: poll latency (seconds) and a
   small persisted watermark. The `lanius code assign` verb is the rejected
   alternative — it re-introduces "issue" into the CLI/kernel surface.

4. **Session ↔ issue linkage lives as a durable lanius-side fact on the session
   (confirm).** The tombstone (M2) and the context stage (M1) both need to know
   *which issue* a session is on. Options: (a) a durable lanius fact/context
   block keyed by session; (b) chainlink's own `session work <id>` state; (c) an
   issue label `session:<id>`. Recommend **(a)**: it is harness-held (survives an
   unclean death — the whole point of M2), and it is what both the stage and the
   tombstone read. Chainlink's `session work` is a *single current* pointer per
   repo and cannot disambiguate the several coding sessions a machine runs at
   once (issue #4 already learned "notes live in context_blocks, not a
   code_sessions column" — same shape of decision). The link is *written* at two
   moments: when the poller delivers an assignment (M3), and when an agent adopts
   an issue via the skill (M1's `chainlink session work` etiquette also calls the
   lanius linkage). Open: exact storage — a `context_blocks`/facts row vs a new
   `code_sessions.linked_issue` column. Recommend a fact row (no schema
   migration, matches #4's precedent).

5. **The kill-9 tombstone rides the EXISTING crash reaper, and this is NOT the
   deferred reconciler (confirm the boundary).** A SIGKILL fires no Stop hook, so
   the clean-exit path (M2, the `session/idle` obs) does not cover it. But the
   dispatcher daemon *already* detects dead sessions every tick —
   `reap_dead_members` (`src/codesession.rs:1463`, called `dispatcher.rs:208`),
   `reap_orphans` (`:200`), and `reap_dead_spawn_edges` (`:267`) which *already
   mails a failure* for a dead unreported worker (test
   `reaper_mails_failure_for_dead_unreported_spawn_worker`, `dispatcher.rs:3084`).
   M2 surfaces that already-existing detection as a **generic, chainlink-ignorant
   harness-vouched death fact** on the bus (a session-exit obs carrying `{session,
   reason, dirty_files}`); the chainlink daemon translates it to a tombstone. The
   kernel emission must name no issue. **This is explicitly not** the deferred
   "bus→chainlink lifecycle bridge daemon (auto-orphan detection)": that deferred
   thing is the *broad reconciler* that sweeps every issue, auto-transitions
   status, and detects orphaned issues with no live session. M2 only stamps one
   truthful comment on a death the harness already saw.

6. **Package home = root `packages/chainlink/` (confirm).** External-CLI bridges
   (discord, telegram, linemux, webhook) all live directly in root `packages/`,
   not in a kit; `recent-history` is the precedent for one package that is BOTH a
   daemon AND a context stage. `chainlink` is that same shape (external CLI +
   skill + stage + daemon) so it belongs alongside them in root `packages/`. The
   alternative — a new `kits/chainlink/` with a bundled profile — only pays off if
   Tim wants it as a shippable opt-in starter pack; the `lanius.toml` is identical
   either way. Recommend root `packages/chainlink/`; it ships **PENDING** and is
   inert until `lanius approve chainlink` (issue state is the owner's, like comms).

7. **Wake target: a noun may have several sessions.** `deliver` addresses a
   specific recorded session; assignment names an agent. The poller maps
   `assignee:<noun>` → the noun's most-recent active recorded coding session
   (`recognize_delivery` only wakes a *known recorded* session). A cold noun with
   no recorded session cannot be started from an assignment in MVP — note it as a
   boundary, don't fake it.

## Milestones

Each milestone is independently landable and independently testable. M1 stands up
the package (skill + stage + daemon skeleton). M2 and M3 each add one boundary
translation to that daemon. M2 and M3 do not depend on each other.

### M1 — the chainlink package: skill, exec wrapper, assigned-issues stage

Create `packages/chainlink/` with `lanius.toml`, `SKILL.md`, and `scripts/`.

- **`SKILL.md` — usage etiquette.** Teach: create/adopt an issue before work
  (`chainlink issue quick "<title>" -p <pri> -l <label>` or `create ... --work`);
  set what you're on (`chainlink session work <id>`, which also records the
  lanius session↔issue link per decision 4); log with the **`--kind`
  vocabulary** — note / plan / decision / observation / blocker / resolution /
  result / handoff / human — and *when to use which* (plan at start, decision on
  a fork, blocker when stuck, result/resolution at close, handoff when passing to
  another agent). Model the prose on the live patterns in issue #4's comments.
  Cross-reference: assignment is `chainlink issue label <id> assignee:<noun>`
  (M3), and the harness stamps a tombstone on death (M2) — the agent need not.
- **Exec authority.** The package ships wrapper `scripts/` that shell the
  `chainlink` binary; per decision (grants section) there is no binary-grant
  field — the wrapper bytes hash into `code_hash`, and the daemon's bus authority
  is the `[request]` publish/subscribe grants M2/M3 need. The agent runs
  `chainlink` via its normal shell surface (the same way workers already run it —
  `.chainlink/hook-config.json` allows the `chainlink ` prefix).
- **Context stage `assigned-issues`.** A `[[stage]] mode="exec"` script that:
  reads the context document on stdin, takes the noun from `doc.meta.agent` and
  the session from `doc.meta.session`, shells `chainlink export -f json` (or
  `chainlink issue list`), and appends ONE `doc.system` block named
  `chainlink-assigned` containing: the issues labeled `assignee:<noun>` (id,
  title, status, priority) and the latest status of the session's linked issue
  (decision 4). Keep it small and read-only; obey the 10s exec budget
  (`context.rs:507`); fail loud but never fabricate (no issues → an honest "no
  assigned issues" block or an omitted block, decided in impl). Model the
  stdin/stdout Doc transform on `clock-block/scripts/clock` and
  `packages/window/scripts/stage`.

**Acceptance:**
- `lanius approve chainlink` then a coding session's next turn shows a
  `chainlink-assigned` system block listing exactly the issues labeled
  `assignee:<that-noun>` and the linked issue's current status — driven by
  `doc.meta.agent`, not any self-report.
- A session with no assignment and no link produces no false rows (honest empty).
- Unit test: feed `run_exec_stage` (or the script directly) a document with a
  known `meta.agent` against a seeded `.chainlink`, assert the block content.
- `chainlink issue comment --kind` usage in `SKILL.md` matches the real binary's
  kind set (guard against drift with a doc test or a grep in CI if cheap).

### M2 — harness-asserted tombstones

When a coding session ends — cleanly, on failure, or by SIGKILL — the harness
(not the agent) stamps one truthful comment on the session's linked issue:
session id, exit reason, and the dirty-file list. This is the M2 trust move from
`worker-dm-unification` applied to status.

- **Death signal, two sources, one shape:**
  - *Clean / failed exit:* `codeagent::hook` already fires on
    `Stop|StopFailure|SessionEnd` and emits `session/idle {event, reason}`
    (`codeagent.rs:9141`, `:9212`). Reuse it — no new hook wiring needed beyond
    ensuring the emission carries (or the consumer can derive) the dirty-file
    list via `lanius code whose --dirty --json` / git status against the recorded
    workdir. Harness-vouched: the reason and dirty list come from harness state,
    never the agent.
  - *SIGKILL / crash:* add a **generic, chainlink-ignorant** death emission to
    the existing reaper (decision 5). `reap_dead_members` / `reap_orphans`
    already detect the corpse each daemon tick; have that path emit a
    session-exit obs `{session, reason: "reaped", dirty_files}`. The kernel names
    no issue.
- **The tombstone consumer** lives in the chainlink daemon (M1's `[process]`).
  It subscribes to the session-exit obs, resolves the session's linked issue
  (decision 4), and runs
  `chainlink issue comment <issue> "session <id> ended: exit=<reason>; dirty=[…]"
  --kind result` (or `--kind handoff` if the session was mid-assignment). It is
  idempotent (one tombstone per session-exit; a watermark like the reaper's
  never-double-mail guard, `dispatcher.rs:3181`). A session with no linked issue
  → no comment (honest skip, logged).

**Acceptance:**
- **The demo:** launch a worker on an issue (link it), `kill -9` the worker
  mid-task, wait one daemon tick, and see a truthful `--kind result` comment
  appear on that issue naming the session id, `reaped`, and the files it had
  dirtied — with the worker never having run a shutdown step.
- A clean `SessionEnd` also produces exactly one tombstone; a session with no
  linked issue produces none.
- The dirty-file list and exit reason are harness-derived (prove it: an agent
  cannot suppress or forge the tombstone by editing its own state).
- Unit test: the death-obs → `chainlink issue comment` mapping (seeded issue +
  synthetic session-exit obs; assert the comment text and `--kind`, and
  idempotency on a repeated obs). e2e: the kill-9 flow above.

### M3 — assignment → mailbox wake (assignment IS dispatch)

Labeling an issue `assignee:<noun>` wakes that agent with the assignment.

- **The assignment poller** lives in the chainlink daemon (M1). Each interval it
  reads `chainlink export -f json`, finds issues carrying an `assignee:<noun>`
  label, and diffs against a persisted watermark of already-notified `(issue,
  noun)` pairs (decision 3). For each *new* assignment it:
  - resolves `<noun>` → its most-recent active recorded coding session
    (decision 7; `recognize_delivery`, `codeagent.rs:527`);
  - records the lanius session↔issue link (decision 4) so M1's stage and M2's
    tombstone see it;
  - emits `lanius code deliver <session> "assigned chainlink issue #<id>:
    <title> — <status>/<priority>"` (`main.rs:1684`), which `drive_code_deliveries`
    (`dispatcher.rs:1395`) drives into the session as a normal mailbox wake.
- Removing the label or reassigning updates the watermark so a later re-assign
  re-wakes. No wake for a cold noun with no recorded session (honest boundary,
  decision 7) — logged, not faked.

**Acceptance:**
- **The demo:** with a recorded worker for noun N, run `chainlink issue label
  <id> assignee:N`; within one poll interval a delivery lands in N's mailbox and
  the worker resumes with the assignment text (verify via the bus/ledger and the
  session's next turn).
- Re-running the same label is a no-op (watermark); removing then re-adding
  re-wakes.
- Assigning to a noun with no recorded session logs a clear "no session to wake"
  and stamps nothing false.
- Unit test: the export→diff→deliver core with a seeded export fixture and a
  fake deliver sink (assert exactly-once emit, correct target, watermark
  behavior). e2e: the label→wake flow above.

## Deferrals And Boundaries

Named here so no one builds them by accident; they are the **follow-up handoff**.

- **The bus→chainlink lifecycle reconciler** — a daemon that watches the bus
  broadly to auto-detect orphaned issues (assigned but no live session),
  auto-transition status, and reconcile drift. M2 stamps a tombstone on a death
  the harness *already* saw; it does not sweep or reconcile. (Decision 5 draws
  the exact line.)
- **The sitrep reconciliation cron sweep** — a periodic "state of all open
  issues" digest. Out.
- **The web-UI issues projection / pane** — surfacing chainlink issues and
  tombstones in the web UI. Out; the read path (`chainlink export -f json`) is
  ready for it when it comes.
- **Cold-start dispatch from assignment** — starting an agent that has never run,
  from an assignment. M3 wakes *recorded* sessions only (decision 7).
- **No kernel change learns "issue".** If a milestone finds itself adding an
  "issue" concept to `broker.rs`, the CLI verb table, or a kernel struct, stop —
  wrong seam. The only kernel touch permitted is the generic session-exit
  emission in the reaper (decision 5), which names no issue.
- Do not edit `worker-dm-unification.md` / `coding-session-reliability.md` or
  their in-flight code; this package is additive.

## Test Strategy

- **Rust unit tests** beside the seams they cover: the reaper session-exit
  emission (M2, next to the existing reaper tests in `dispatcher.rs`), and any
  linkage-fact read/write helper. Keep them env-free and seeded (a temp root + a
  temp `.chainlink`), mirroring the existing `codesession`/`dispatcher` test
  style.
- **Stage/script tests** (M1): drive the stage script with a crafted document on
  stdin against a seeded `.chainlink`; assert the produced block. Python or a
  Rust `run_exec_stage` harness, whichever the script language is.
- **Poller/consumer tests** (M2 consumer, M3 poller): feed a `chainlink export
  -f json` fixture + a synthetic session-exit obs; assert exactly-once
  comment/deliver, correct `--kind`/target, and watermark idempotency. Do **not**
  poll a live `.chainlink` in unit tests.
- **e2e (the two demos):** kill-9 → tombstone (M2); label → wake (M3). Run these
  against a real dispatcher daemon in a throwaway root, never the live repo's
  `.chainlink`. Kill any daemon a test starts.
- Run focused tests, then the full `cargo test`; report counts. If the stage
  script is Python, add it to the relevant python test run.

## Log

- 2026-07-10 (planner, session c185ae6f) — Recon against the installed
  `chainlink 0.2.0` and the live codebase. Key findings that shaped the plan:
  (1) chainlink has **no** command-triggering hook (`.chainlink/hook-config.json`
  is git-command tracking chainlink injects into Claude Code) and **no** `issue
  assign` subcommand — so assignment is modeled as a label and the wake is a
  poller, not a chainlink hook (decisions 1–3). (2) The context stage receives
  the agent noun via `doc.meta.agent` on stdin, not an env var
  (`context.rs:479-526`) — M1's stage keys off that. (3) The kill-9 tombstone can
  ride the existing crash reaper (`reap_dead_members`, `dispatcher.rs:208`;
  `reap_dead_spawn_edges` already mails failure for dead workers) rather than a
  new orphan-detector, keeping the broad reconciler genuinely deferred
  (decision 5). (4) `lanius code deliver` + `drive_code_deliveries` is the ready
  wake primitive (M3). (5) `recent-history` is the precedent for one package
  that is both a daemon and a context stage; discord/telegram are the
  external-CLI-bridge-in-root-`packages/` precedent — chainlink is that shape
  (decision 6). (6) `chainlink export -f json` exposes id/title/status/labels/
  comments/updated_at — a stable read interface for the poller. Status: proposed,
  pending Fable review before implementation.
</content>
</invoke>
